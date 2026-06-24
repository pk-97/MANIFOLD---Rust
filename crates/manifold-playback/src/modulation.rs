//! Modulation pipeline — per-frame driver (LFO) and envelope (decay) evaluation.
//!
//! Port of C# DriverController + ParameterDriverManager + EnvelopeEvaluator.
//!
//! Execution order each frame (after SyncClipsToTime, before compositor):
//!   1. reset_all_effectives(project)         — base → effective
//!   2. evaluate_all_drivers(project, beat)    — LFO → effective
//!   3. evaluate_all_envelopes(project, beat)  — decay → effective (additive);
//!      one walk visits every layer's effects AND its generator instance
//!      (the former separate `evaluate_gen_param_envelopes` pass is folded in)
//!   4. If any_dirty → mark compositor dirty
//!
//! Envelopes are clip-triggered decays: depth is the per-envelope
//! `target_normalized` ("Amount"), the fall-off is the fixed `ENVELOPE_DECAY_BEATS`
//! feel. The level is a pure function of beats-into-clip, so the walk holds no
//! per-frame envelope state — a clip loop re-triggers naturally as elapsed resets.

use std::collections::HashMap;

use manifold_core::audio_features::{AudioFeatureSnapshot, SendFeatures};
use manifold_core::id::AudioSendId;
use manifold_core::{Beats, Seconds};
use manifold_core::effects::{PresetInstance, ParamEnvelope, ParameterDriver};
use manifold_core::preset_definition_registry;
use manifold_core::project::Project;
use manifold_core::types::LayerType;

// ── Shared modulation core ──────────────────────────────────────────────────
//
// The effect-side and generator-side walks below differ only in how they
// *resolve* a target param (effect: registry + user-binding tail; generator:
// registry only) and in their outer iteration (effects locate a target effect
// by type within the layer; generators operate on the single gen-param state).
// The arithmetic that maps a driver/envelope onto a slot value is identical on
// both sides. These helpers hold that arithmetic in exactly one place so a
// modulation fix lands once. Byte-for-byte the prior inline logic — extracted,
// not changed.

/// Map a driver's normalized output onto a target parameter's value range.
fn driver_target_value(driver: &ParameterDriver, current_beat: Beats, min: f32, max: f32) -> f32 {
    // `period_beats()` is the free period when the driver is in free mode, else
    // the sync division's period (dotted/triplet baked into the variant).
    let mut normalized = ParameterDriver::evaluate_with_period(
        current_beat,
        driver.period_beats(),
        driver.waveform,
        driver.phase,
    );
    if driver.reversed {
        normalized = 1.0 - normalized;
    }
    // Apply trim: map [0,1] to [lo, hi] within param range
    let lo = min + (max - min) * driver.trim_min;
    let hi = min + (max - min) * driver.trim_max;
    lo + (hi - lo) * normalized
}

/// Apply an envelope's additive decay offset to a single param slot value.
/// `level` is the decay curve [0,1]; the value is pulled `level` of the way from
/// its base toward the depth target. Returns true if the value changed.
fn apply_envelope_offset(value: &mut f32, min: f32, max: f32, target_norm: f32, level: f32) -> bool {
    let current = *value;
    let target = min + (max - min) * target_norm.clamp(0.0, 1.0);
    let offset = (target - current) * level;
    let final_value = (current + offset).clamp(min, max);
    if (final_value - current).abs() > f32::EPSILON {
        *value = final_value;
        true
    } else {
        false
    }
}

/// Apply every decay envelope carried by one instance against the active-clip
/// timing of the container it lives in. Returns true if any envelope wrote a
/// value this frame.
///
/// Since envelope-home unification this is the single envelope walk for both
/// kinds: an envelope lives on its owning `PresetInstance`, and
/// `resolve_param_in` resolves a param id against either an effect or a
/// generator definition — so the two formerly-parallel blocks collapse to a
/// caller that locates the def + timing and hands off here. The level is a pure
/// function of `active_elapsed`, so the walk reads envelopes immutably and only
/// mutates `param_values` — no per-frame envelope bookkeeping.
fn apply_instance_envelopes(
    inst: &mut PresetInstance,
    def: &manifold_core::preset_def::PresetDef,
    active_elapsed: Beats,
) -> bool {
    if active_elapsed < Beats::ZERO {
        return false; // no active clip → no trigger
    }
    let env_count = inst.envelopes.as_ref().map_or(0, |e| e.len());
    let mut any_modulated = false;

    for ei in 0..env_count {
        let (enabled, param_id, target_norm, decay_beats) = {
            let env = &inst.envelopes.as_ref().unwrap()[ei];
            (
                env.enabled,
                env.param_id.clone(),
                env.target_normalized,
                env.decay_beats,
            )
        };
        if !enabled {
            continue;
        }
        let Some(resolved) =
            manifold_core::effects::resolve_param_in(def, inst, param_id.as_ref())
        else {
            continue;
        };
        if resolved.idx >= inst.param_values.len() {
            continue;
        }
        let level = ParamEnvelope::decay_level(active_elapsed, decay_beats);
        if apply_envelope_offset(
            &mut inst.param_values[resolved.idx].value,
            resolved.min,
            resolved.max,
            target_norm,
            level,
        ) {
            any_modulated = true;
        }
    }

    any_modulated
}

// =====================================================================
// Phase 1: Reset all effectives (base → effective, blank slate)
// Port of C# DriverController.ResetAllEffectives()
// =====================================================================

/// Reset all effect and generator param effective values to their base values.
/// Called once per frame before drivers and envelopes are applied.
pub fn reset_all_effectives(project: &mut Project) {
    let layers = &mut project.timeline.layers;
    for layer in layers.iter_mut() {
        // Generator params: reset only driven/enveloped params (per Unity semantics)
        if layer.layer_type == LayerType::Generator
            && let Some(gp) = layer.gen_params_mut()
        {
            gp.reset_effectives();
        }

        // Layer effect params: reset all (copy base → effective)
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                fx.reset_param_effectives();
            }
        }
    }

    // Master effect params: reset all
    for fx in project.settings.master_effects.iter_mut() {
        fx.reset_param_effectives();
    }
}

// =====================================================================
// Phase 2: Evaluate all parameter drivers (LFOs)
// Port of C# ParameterDriverManager.EvaluateAll()
// =====================================================================

/// Evaluate all parameter drivers on master effects, layer effects, and generator params.
/// Returns true if any driver was active (compositor should be marked dirty).
pub fn evaluate_all_drivers(project: &mut Project, current_beat: Beats) -> bool {
    let mut any_driven = false;

    // Master effect drivers
    for fx in project.settings.master_effects.iter_mut() {
        if evaluate_instance_drivers(fx, current_beat) {
            any_driven = true;
        }
    }

    // Layer effect drivers + generator param drivers.
    // Modulation runs even on muted layers — mute only suppresses compositor
    // output, not the modulation pipeline. This keeps the inspector showing
    // live driver/envelope values regardless of mute state.
    for layer in project.timeline.layers.iter_mut() {
        // Layer effect drivers
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if evaluate_instance_drivers(fx, current_beat) {
                    any_driven = true;
                }
            }
        }

        // Generator param drivers — same path effect drivers take (also picks
        // up user-tail driver bindings that the former inline id_to_index walk
        // missed).
        if let Some(gp) = layer.gen_params_mut()
            && evaluate_instance_drivers(gp, current_beat)
        {
            any_driven = true;
        }
    }

    any_driven
}

/// Evaluate all drivers on a single PresetInstance. Returns true if any driver was active.
/// Port of C# ParameterDriverManager.EvaluateEffectDrivers().
fn evaluate_instance_drivers(fx: &mut PresetInstance, current_beat: Beats) -> bool {
    if !fx.enabled {
        return false;
    }
    let drivers = match &fx.drivers {
        Some(d) if !d.is_empty() => d,
        _ => return false,
    };

    let effect_def = match preset_definition_registry::try_get(fx.effect_type()) {
        Some(d) => d,
        None => return false,
    };
    let mut any_driven = false;

    // Collect results to avoid borrow conflict between drivers and param_values
    let results: Vec<(usize, f32)> = drivers
        .iter()
        .filter(|d| d.enabled && !d.is_paused_by_user)
        .filter_map(|driver| {
            let resolved = manifold_core::effects::resolve_param_in(
                &effect_def,
                fx,
                driver.param_id.as_ref(),
            )?;
            let (idx, min, max) = (resolved.idx, resolved.min, resolved.max);
            Some((idx, driver_target_value(driver, current_beat, min, max)))
        })
        .collect();

    for (idx, value) in results {
        if idx < fx.param_values.len() {
            fx.param_values[idx].value = value;
            any_driven = true;
        }
    }

    any_driven
}

// =====================================================================
// Phase 3: Evaluate all ADSR envelopes on clip/layer effects
// Port of C# EnvelopeEvaluator.EvaluateAll()
// =====================================================================

/// Evaluate every layer effect's envelopes (ADSR + Random).
/// Returns true if any envelope was active (compositor should be marked dirty).
///
/// Envelope-home unification: envelopes ride on each effect's
/// `PresetInstance.envelopes` (keyed by `param_id`), so this just walks the
/// layer's effects and applies each instance's own envelopes against that
/// layer's active-clip timing — no more `(effect_type, param_id)` pool match,
/// which means two same-type effects on one layer no longer collide.
/// Disabled effects are skipped (an envelope on a disabled effect doesn't
/// modulate), matching the prior `find(enabled)` gate. Modulation runs even
/// on muted layers (mute = compositor only). Master + clip effect envelopes
/// have no clip-timing source and stay inert, exactly as before.
pub fn evaluate_all_envelopes(
    project: &mut Project,
    active_clip_timing: &[(Beats, Beats)],
) -> bool {
    let mut any_modulated = false;

    for (li, layer) in project.timeline.layers.iter_mut().enumerate() {
        // Only the elapsed-into-clip drives the decay level; duration is unused.
        let active_elapsed = active_clip_timing
            .get(li)
            .map(|(elapsed, _dur)| *elapsed)
            .unwrap_or(Beats(-1.0));

        // Layer effects.
        if let Some(effects) = layer.effects.as_mut() {
            for fx in effects.iter_mut() {
                if !fx.enabled {
                    continue;
                }
                let Some(def) = preset_definition_registry::try_get(fx.effect_type()) else {
                    continue;
                };
                if apply_instance_envelopes(fx, &def, active_elapsed) {
                    any_modulated = true;
                }
            }
        }

        // Generator instance (the layer's singleton gen_params) — the same
        // walk, no separate generator pass.
        if let Some(gp) = layer.gen_params_mut()
            && let Some(def) = preset_definition_registry::try_get(gp.generator_type())
            && apply_instance_envelopes(gp, &def, active_elapsed)
        {
            any_modulated = true;
        }
    }

    any_modulated
}

// =====================================================================
// Top-level orchestration — called from engine tick
// Port of C# DriverController.Update()
// =====================================================================

/// Run the full modulation pipeline: reset → drivers → envelopes → gen envelopes.
/// Returns true if any modulation was applied (compositor should be marked dirty).
pub fn evaluate_modulation(
    project: &mut Project,
    current_beat: Beats,
    dt: Seconds,
    audio: &AudioFeatureSnapshot,
    timing_scratch: &mut Vec<(Beats, Beats)>,
) -> bool {
    // Phase 1: Reset all effective values to base
    reset_all_effectives(project);

    // Phase 2: Evaluate LFO drivers
    let any_driven = evaluate_all_drivers(project, current_beat);

    // Phase 2.5: Evaluate audio modulations (live audio → effective). Driver-
    // like (sets the value), so it runs alongside drivers and before the
    // additive envelope phase. Inert when no audio features are present.
    let any_audio = evaluate_all_audio_mods(project, audio, dt);

    // Pre-compute per-layer active clip timing for envelope phases.
    // Avoids O(total_clips) scan in each envelope function.
    compute_active_clip_timing(&project.timeline.layers, current_beat, timing_scratch);

    // Phase 3: Evaluate clip/layer/generator ADSR envelopes (additive on top
    // of drivers). One walk visits every layer's effects AND its generator
    // instance — see evaluate_all_envelopes.
    let any_enveloped = evaluate_all_envelopes(project, timing_scratch);

    any_driven || any_audio || any_enveloped
}

// =====================================================================
// Phase 2.5: Evaluate audio modulations (live audio features → effective)
// =====================================================================

/// Evaluate every audio modulation on master effects, layer effects, and
/// generator params, using the latest per-send feature `snapshot`. Returns true
/// if any modulation wrote a value (compositor should be marked dirty).
///
/// Walks the same instance set as the driver pass (master + layer effects +
/// generators) — NOT clip effects, which the modulation pipeline does not reset
/// or evaluate. A modulation whose send no longer resolves (deleted send, or no
/// features yet) is skipped, leaving its param at the base value from
/// `reset_all_effectives` — the orphan policy, matching drivers/envelopes.
pub fn evaluate_all_audio_mods(
    project: &mut Project,
    snapshot: &AudioFeatureSnapshot,
    dt: Seconds,
) -> bool {
    if snapshot.is_empty() || project.audio_setup.sends.is_empty() {
        return false;
    }

    // Resolve send id → features once (owned), from the AudioSetup's send order
    // (the worker's frame index). Built under an immutable borrow that ends
    // before the mutable instance walk below.
    let mut by_send: HashMap<AudioSendId, SendFeatures> = HashMap::new();
    for (i, send) in project.audio_setup.sends.iter().enumerate() {
        if let Some(f) = snapshot.get(i) {
            by_send.insert(send.id.clone(), *f);
        }
    }
    if by_send.is_empty() {
        return false;
    }

    let mut any = false;

    for fx in project.settings.master_effects.iter_mut() {
        if evaluate_instance_audio_mods(fx, &by_send, dt) {
            any = true;
        }
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if evaluate_instance_audio_mods(fx, &by_send, dt) {
                    any = true;
                }
            }
        }
        if let Some(gp) = layer.gen_params_mut()
            && evaluate_instance_audio_mods(gp, &by_send, dt)
        {
            any = true;
        }
    }

    any
}

/// Evaluate every audio modulation on a single instance. Returns true if any
/// wrote a value.
fn evaluate_instance_audio_mods(
    fx: &mut PresetInstance,
    by_send: &HashMap<AudioSendId, SendFeatures>,
    dt: Seconds,
) -> bool {
    if !fx.enabled {
        return false;
    }
    if !fx.has_audio_mods() {
        return false;
    }
    let def = match preset_definition_registry::try_get(fx.effect_type()) {
        Some(d) => d,
        None => return false,
    };

    // Pass 1 (immutable): resolve each enabled mod to its slot + raw feature.
    let mods = fx.audio_mods.as_ref().unwrap();
    let work: Vec<(usize, usize, f32, f32, f32)> = mods
        .iter()
        .enumerate()
        .filter(|(_, m)| m.enabled)
        .filter_map(|(mi, m)| {
            let features = by_send.get(&m.source.send_id)?;
            let resolved =
                manifold_core::effects::resolve_param_in(&def, fx, m.param_id.as_ref())?;
            if resolved.idx >= fx.param_values.len() {
                return None;
            }
            let raw = m.source.feature.extract(features);
            Some((mi, resolved.idx, resolved.min, resolved.max, raw))
        })
        .collect();
    if work.is_empty() {
        return false;
    }

    // Pass 2 (mutable): shape (advancing the follower state) and write.
    let dt_s = dt.0 as f32;
    for (mi, idx, min, max, raw) in work {
        let out_norm = {
            let mods = fx.audio_mods.as_mut().unwrap();
            let m = &mut mods[mi];
            let shape = m.shape;
            shape.apply(raw, dt_s, &mut m.smoothed, &mut m.prev_raw)
        };
        fx.param_values[idx].value = min + (max - min) * out_norm;
    }
    true
}

/// Per-layer active clip timing: (elapsed, duration).
/// Sentinel `Beats(-1.0)` for elapsed means no active clip on that layer.
fn compute_active_clip_timing(
    layers: &[manifold_core::layer::Layer],
    current_beat: Beats,
    timing: &mut Vec<(Beats, Beats)>,
) {
    timing.clear();
    for layer in layers {
        let mut elapsed = Beats(-1.0);
        let mut duration = Beats::ZERO;
        for clip in &layer.clips {
            if clip.is_muted {
                continue;
            }
            let e = current_beat - clip.start_beat;
            if e >= Beats::ZERO && e < clip.duration_beats {
                elapsed = e;
                duration = clip.duration_beats;
                break;
            }
        }
        timing.push((elapsed, duration));
    }
}

// =====================================================================
// Characterization tests for the per-frame envelope evaluators.
//
// These pin the *current* behavior of `evaluate_all_envelopes` — the single
// walk that now visits both effect targets and generator params (the former
// separate `evaluate_gen_param_envelopes` is folded in) — across the
// envelope-home unification that moved effect envelopes off `layer.envelopes`
// (keyed by `target_effect_type`) and onto each effect's own
// `PresetInstance.envelopes`. The arithmetic (ADSR offset, random walk) is
// invariant across that refactor; the effect-target *resolution* is the part
// that changes, and `effect_envelope_resolves_target_by_type_first_match`
// documents the leak the move fixes.
//
// manifold-renderer (which submits the real shipping presets) isn't linked
// into the manifold-playback test binary, so the evaluators would resolve
// nothing. We register one synthetic effect and one synthetic generator here
// via `inventory` so `resolve_param_in` / the generator registry have a
// target — the same fixture pattern manifold-core's own registry tests use.
// =====================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_registration::EffectMetadata;
    use manifold_core::effects::{DEFAULT_ENVELOPE_DECAY_BEATS, ParamEnvelope};
    use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};
    use manifold_core::layer::Layer;
    use manifold_core::preset_definition_registry::create_default;
    use manifold_core::project::Project;
    use manifold_core::PresetTypeId;

    const TEST_FX: PresetTypeId = PresetTypeId::new("TestEnvFx");
    const TEST_GEN: PresetTypeId = PresetTypeId::new("TestEnvGen");

    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::new("TestEnvFx"),
            display_name: "Test Env Fx",
            category: "Test",
            available: true,
            osc_prefix: "testEnvFx",
            legacy_discriminant: None,
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole("segs", "Segs", 0.0, 8.0, 0.0, ""),
            ],
        }
    }
    inventory::submit! {
        GeneratorMetadata {
            id: PresetTypeId::new("TestEnvGen"),
            display_name: "Test Env Gen",
            is_line_based: false,
            available: true,
            osc_prefix: "testEnvGen",
            legacy_discriminant: None,
            params: &[
                ParamSpec::continuous("speed", "Speed", 0.0, 10.0, 0.0, "F2", ""),
                ParamSpec::whole("count", "Count", 0.0, 8.0, 0.0, ""),
            ],
        }
    }

    /// A video layer carrying one default `TestEnvFx` effect instance.
    fn layer_with_one_effect() -> Layer {
        let mut layer = Layer::new_video("FxLayer".into(), 0);
        layer.effects = Some(vec![create_default(&TEST_FX)]);
        layer
    }

    /// A generator layer whose `gen_params` is initialized to `TestEnvGen`
    /// registry defaults (populated `param_values`, kind = Generator).
    fn generator_layer() -> Layer {
        let mut layer = Layer::new_generator("GenLayer".into(), TEST_GEN.clone(), 0);
        layer.gen_params_or_init().init_defaults_for_type(TEST_GEN.clone());
        layer
    }

    /// Full-depth decay envelope. At the clip's rising edge (elapsed 0) the decay
    /// level is 1.0, so it applies the full offset toward `target_normalized`.
    fn full_depth_env(param: &'static str) -> ParamEnvelope {
        let mut env = ParamEnvelope::new(param);
        env.target_normalized = 1.0;
        env
    }

    /// A video layer with one `TestEnvFx` effect carrying `env` on the
    /// instance — the post-unification home for effect envelopes.
    fn effect_layer_with_env(env: ParamEnvelope) -> Layer {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].envelopes = Some(vec![env]);
        layer
    }

    fn project_with(layer: Layer) -> Project {
        let mut project = Project::default();
        project.timeline.layers = vec![layer];
        project
    }

    // ── effect decay envelope ────────────────────────────────────────────

    #[test]
    fn effect_envelope_applies_full_offset_at_trigger() {
        let layer = effect_layer_with_env(full_depth_env("amount"));
        let mut project = project_with(layer);

        // Rising edge (elapsed 0) → decay level 1.0 → full offset.
        let timing = vec![(Beats(0.0), Beats(8.0))];
        let modulated = evaluate_all_envelopes(&mut project, &timing);

        assert!(modulated, "an envelope at its trigger reports modulation");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].value - 1.0).abs() < 1e-6,
            "amount driven from base 0.0 to max 1.0, got {}",
            fx.param_values[0].value
        );
    }

    #[test]
    fn effect_envelope_partial_target_normalized() {
        let mut env = full_depth_env("amount");
        env.target_normalized = 0.5; // target = min + (max-min)*0.5 = 0.5
        let layer = effect_layer_with_env(env);
        let mut project = project_with(layer);

        let timing = vec![(Beats(0.0), Beats(8.0))];
        assert!(evaluate_all_envelopes(&mut project, &timing));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].value - 0.5).abs() < 1e-6,
            "amount driven to half-range 0.5, got {}",
            fx.param_values[0].value
        );
    }

    #[test]
    fn effect_envelope_decays_over_decay_beats() {
        // Per-envelope decay time: halfway through the window the level is 0.5,
        // past it, 0. Uses the default decay (1.0 beat) from `full_depth_env`.
        let layer = effect_layer_with_env(full_depth_env("amount"));
        let mut project = project_with(layer);

        let half = (DEFAULT_ENVELOPE_DECAY_BEATS * 0.5) as f64;
        assert!(evaluate_all_envelopes(&mut project, &[(Beats(half), Beats(8.0))]));
        let v_half = project.timeline.layers[0].effects.as_ref().unwrap()[0].param_values[0].value;
        assert!(
            (v_half - 0.5).abs() < 1e-5,
            "half-decay drives amount to ~0.5, got {v_half}"
        );

        // Reset base, then past the decay window → level 0 → no change.
        project.timeline.layers[0].effects.as_mut().unwrap()[0].param_values[0].value = 0.0;
        let past = (DEFAULT_ENVELOPE_DECAY_BEATS + 0.5) as f64;
        assert!(!evaluate_all_envelopes(&mut project, &[(Beats(past), Beats(8.0))]));
        let v_past = project.timeline.layers[0].effects.as_ref().unwrap()[0].param_values[0].value;
        assert_eq!(v_past, 0.0, "past the decay window the envelope is spent");
    }

    #[test]
    fn effect_envelope_no_change_when_clip_inactive() {
        let layer = effect_layer_with_env(full_depth_env("amount"));
        let mut project = project_with(layer);

        // No active clip on this layer (sentinel elapsed -1).
        let timing = vec![(Beats(-1.0), Beats::ZERO)];
        let modulated = evaluate_all_envelopes(&mut project, &timing);

        assert!(!modulated, "no active clip => no modulation");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0, "param untouched");
    }

    #[test]
    fn effect_disabled_envelope_is_noop() {
        let mut env = full_depth_env("amount");
        env.enabled = false;
        let layer = effect_layer_with_env(env);
        let mut project = project_with(layer);

        let timing = vec![(Beats(0.0), Beats(8.0))];
        assert!(!evaluate_all_envelopes(&mut project, &timing));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0);
    }

    #[test]
    fn envelope_on_disabled_effect_is_noop() {
        // An envelope riding on a disabled effect doesn't modulate (matches the
        // prior find(enabled) gate, now expressed as "skip disabled effects").
        let mut layer = effect_layer_with_env(full_depth_env("amount"));
        layer.effects.as_mut().unwrap()[0].enabled = false;
        let mut project = project_with(layer);

        let timing = vec![(Beats(0.0), Beats(8.0))];
        assert!(!evaluate_all_envelopes(&mut project, &timing));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0);
    }

    /// Post-unification behavior (the bug fix): an envelope binds to the
    /// instance it sits on, so two same-type effects no longer collide —
    /// each effect's own envelope modulates only that effect.
    #[test]
    fn envelopes_are_per_instance_not_pooled_by_type() {
        let mut layer = Layer::new_video("FxLayer".into(), 0);
        let mut fx_a = create_default(&TEST_FX);
        let mut fx_b = create_default(&TEST_FX);
        // Only the SECOND same-type effect carries an envelope.
        fx_a.envelopes = None;
        fx_b.envelopes = Some(vec![full_depth_env("amount")]);
        layer.effects = Some(vec![fx_a, fx_b]);
        let mut project = project_with(layer);

        let timing = vec![(Beats(0.0), Beats(8.0))];
        assert!(evaluate_all_envelopes(&mut project, &timing));
        let effects = project.timeline.layers[0].effects.as_ref().unwrap();
        assert_eq!(
            effects[0].param_values[0].value, 0.0,
            "the effect WITHOUT an envelope is untouched"
        );
        assert!(
            (effects[1].param_values[0].value - 1.0).abs() < 1e-6,
            "the effect WITH the envelope is the one modulated"
        );
    }

    // ── generator decay envelope ─────────────────────────────────────────

    #[test]
    fn generator_envelope_applies_full_offset() {
        let mut layer = generator_layer();
        {
            let gp = layer.gen_params_or_init();
            let mut env = ParamEnvelope::new("speed");
            env.target_normalized = 1.0;
            gp.envelopes = Some(vec![env]);
        }
        let mut project = project_with(layer);

        let timing = vec![(Beats(0.0), Beats(8.0))];
        let modulated = evaluate_all_envelopes(&mut project, &timing);

        assert!(modulated);
        let gp = project.timeline.layers[0].gen_params().unwrap();
        assert!(
            (gp.param_values[0].value - 10.0).abs() < 1e-6,
            "speed driven from 0 to max 10, got {}",
            gp.param_values[0].value
        );
    }

    #[test]
    fn generator_disabled_envelope_is_noop() {
        let mut layer = generator_layer();
        {
            let gp = layer.gen_params_or_init();
            let mut env = ParamEnvelope::new("speed");
            env.enabled = false;
            gp.envelopes = Some(vec![env]);
        }
        let mut project = project_with(layer);

        let timing = vec![(Beats(0.0), Beats(8.0))];
        assert!(!evaluate_all_envelopes(&mut project, &timing));
        let gp = project.timeline.layers[0].gen_params().unwrap();
        assert_eq!(gp.param_values[0].value, 0.0);
    }

    #[test]
    fn generator_envelope_only_runs_on_generator_layers() {
        // An effect layer carrying a gen-style envelope on its (absent)
        // gen_params is never reached by the generator evaluator.
        let layer = layer_with_one_effect();
        let mut project = project_with(layer);
        let timing = vec![(Beats(0.0), Beats(8.0))];
        assert!(!evaluate_all_envelopes(&mut project, &timing));
    }

    // ── audio modulation ─────────────────────────────────────────────────

    use manifold_core::audio_features::{AudioFeatureSnapshot, SendFeatures};
    use manifold_core::audio_mod::{
        AudioBand, AudioFeature, AudioFeatureKind, AudioModShape, ParameterAudioMod,
    };
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::id::AudioSendId;
    use manifold_core::Seconds;

    /// A project: one effect layer with `TestEnvFx`, plus an AudioSetup with a
    /// single send "Bass". Returns (project, send_id).
    fn project_with_audio_send() -> (Project, AudioSendId) {
        let mut project = project_with(layer_with_one_effect());
        let send = AudioSend::new("Bass");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        (project, send_id)
    }

    /// A snapshot with one send whose low-band amplitude reads `low`.
    fn snapshot_low(low: f32) -> AudioFeatureSnapshot {
        let mut bands = [manifold_core::BandFeatures::default(); 4];
        bands[manifold_core::AudioBand::Low.index()].amplitude = low;
        AudioFeatureSnapshot {
            sends: vec![SendFeatures { bands, ..Default::default() }],
        }
    }

    /// Attach an audio mod on `amount` reading the send's low-band amplitude,
    /// snapping instantly (no smoothing) over the full range.
    fn attach_full_range_low_mod(project: &mut Project, send_id: &AudioSendId) {
        let mut m = ParameterAudioMod::new(
            "amount".into(),
            send_id.clone(),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low),
        );
        m.shape = AudioModShape {
            attack_ms: 0.0,
            release_ms: 0.0,
            ..Default::default()
        };
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods_mut()
            .push(m);
    }

    #[test]
    fn audio_mod_drives_param_from_band_energy() {
        let (mut project, send_id) = project_with_audio_send();
        attach_full_range_low_mod(&mut project, &send_id);

        let snap = snapshot_low(1.0);
        let active = evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016));
        assert!(active, "an audio mod with signal reports modulation");

        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        // amount range 0..1, full-range mapping, raw 1.0 → 1.0.
        assert!(
            (fx.param_values[0].value - 1.0).abs() < 1e-6,
            "amount driven to 1.0, got {}",
            fx.param_values[0].value
        );
    }

    #[test]
    fn audio_mod_inert_when_send_missing_from_snapshot() {
        let (mut project, send_id) = project_with_audio_send();
        attach_full_range_low_mod(&mut project, &send_id);

        // Empty snapshot → no features → no write.
        let empty = AudioFeatureSnapshot::default();
        assert!(!evaluate_all_audio_mods(&mut project, &empty, Seconds(0.016)));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0, "param untouched");
    }

    #[test]
    fn audio_mod_skipped_when_disabled() {
        let (mut project, send_id) = project_with_audio_send();
        attach_full_range_low_mod(&mut project, &send_id);
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .enabled = false;

        let snap = snapshot_low(1.0);
        assert!(!evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016)));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0);
    }

    #[test]
    fn audio_mod_orphaned_send_leaves_param_at_base() {
        // A mod referencing a send id that isn't in the setup is inert.
        let mut project = project_with(layer_with_one_effect());
        attach_full_range_low_mod(&mut project, &AudioSendId::new("ghost"));
        // Setup has a different send, so the snapshot's send 0 won't match.
        project.audio_setup.sends.push(AudioSend::new("Other"));

        let snap = snapshot_low(1.0);
        assert!(!evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016)));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0);
    }

    /// Overlay a recalibrated range on a STOCK param via the instance's graph
    /// `preset_metadata` — exactly what the chevron popover writes. The registry
    /// def still owns the param; this only narrows its range on this instance.
    fn override_stock_param_range(inst: &mut PresetInstance, param_id: &str, min: f32, max: f32) {
        use manifold_core::effect_graph_def::{EffectGraphDef, ParamSpecDef, PresetMetadata};
        let graph = inst.graph.get_or_insert_with(|| EffectGraphDef {
            version: 0,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: Vec::new(),
            wires: Vec::new(),
        });
        let meta = graph.preset_metadata.get_or_insert_with(|| PresetMetadata {
            id: PresetTypeId::new(""),
            display_name: String::new(),
            category: String::new(),
            osc_prefix: String::new(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            skip_mode: Default::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        });
        meta.params.push(ParamSpecDef {
            id: param_id.to_string(),
            name: param_id.to_string(),
            min,
            max,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
        });
    }

    #[test]
    fn audio_mod_respects_recalibrated_range_override() {
        // Narrow `amount`'s range to 0..0.5 on this instance (what the chevron
        // popover writes to preset_metadata). A full-signal audio mod must cap at
        // the override max 0.5, not the catalog max 1.0 — the resolver is
        // override-first, so a recalibrated slider bounds the modulator too.
        let (mut project, send_id) = project_with_audio_send();
        attach_full_range_low_mod(&mut project, &send_id);
        override_stock_param_range(
            &mut project.timeline.layers[0].effects.as_mut().unwrap()[0],
            "amount",
            0.0,
            0.5,
        );

        let snap = snapshot_low(1.0);
        assert!(evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016)));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].value - 0.5).abs() < 1e-6,
            "full signal caps at the recalibrated max 0.5, got {}",
            fx.param_values[0].value,
        );
    }
}
