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
/// kinds: an envelope lives on its owning `PresetInstance` and resolves its
/// target param directly against that instance's manifest (id-keyed, range +
/// integral-ness self-contained on each `Param.spec`) — so effects and
/// generators share one walk with no registry consultation. The level is a pure
/// function of `active_elapsed`, so the walk reads envelopes immutably and only
/// mutates the manifest — no per-frame envelope bookkeeping.
fn apply_instance_envelopes(
    inst: &mut PresetInstance,
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
        let Some(p) = inst.params.get_mut(param_id.as_ref()) else {
            continue;
        };
        let (min, max) = (p.spec.min, p.spec.max);
        let level = ParamEnvelope::decay_level(active_elapsed, decay_beats);
        if apply_envelope_offset(&mut p.value, min, max, target_norm, level) {
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
    // Take the drivers out so the manifest can be written in the same pass
    // without the old per-frame `Vec<(usize, f32)>` scratch (it allocated every
    // frame on this hot path). No registry def needed: each `Param` carries its
    // own reshaped range (calibration edits `spec.min`/`spec.max` in place), so
    // a driver denormalizes against `p.spec` directly — which also removes the
    // stale-registry-index misroute the manifest redesign targets.
    let drivers = fx.drivers.take();
    let mut any_driven = false;
    if let Some(ds) = &drivers {
        for driver in ds.iter().filter(|d| d.enabled && !d.is_paused_by_user) {
            if let Some(p) = fx.params.get_mut(driver.param_id.as_ref()) {
                let (min, max) = (p.spec.min, p.spec.max);
                p.value = driver_target_value(driver, current_beat, min, max);
                any_driven = true;
            }
        }
    }
    fx.drivers = drivers;
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
                if apply_instance_envelopes(fx, active_elapsed) {
                    any_modulated = true;
                }
            }
        }

        // Generator instance (the layer's singleton gen_params) — the same
        // walk, no separate generator pass.
        if let Some(gp) = layer.gen_params_mut()
            && apply_instance_envelopes(gp, active_elapsed)
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
    trigger_pulses: &mut Vec<TriggerPulse>,
) -> bool {
    // Phase 1: Reset all effective values to base
    reset_all_effectives(project);

    // Phase 2: Evaluate LFO drivers
    let any_driven = evaluate_all_drivers(project, current_beat);

    // Phase 2.5: Evaluate audio modulations (live audio → effective). Driver-
    // like (sets the value), so it runs alongside drivers and before the
    // additive envelope phase. Inert when no audio features are present.
    let any_audio = evaluate_all_audio_mods(project, audio, dt);

    // Phase 2.6 (§8): each instance's own `audio_trigger` config (D2) fires
    // independently of the continuous audio-mod pass above — surfaced as a
    // per-layer/master pulse list for the renderer (P2) to fold into
    // `audio_count`. Never marks the compositor dirty on its own (the
    // renderer's own dirty tracking covers the visible effect); collected
    // here so the evaluation stays a single walk per tick.
    *trigger_pulses = evaluate_all_param_triggers(project, audio);

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

    // Single pass over two disjoint fields: `audio_mods` (mutable — each active
    // follower advances its smoothing state) and `params` (the id-keyed
    // manifest). This drops the old per-frame scratch `Vec` and the registry
    // def lookup; the range is self-contained on each `Param.spec`, so there is
    // no positional slot to resolve. Existence is checked (via the range read)
    // BEFORE the follower advances, matching the old pass-1 filter so an
    // unresolved mod does not advance its smoothing.
    let dt_s = dt.0 as f32;
    let mods = fx.audio_mods.as_mut().unwrap();
    let params = &mut fx.params;
    let mut wrote = false;
    for m in mods.iter_mut().filter(|m| m.enabled) {
        let Some(features) = by_send.get(&m.source.send_id) else {
            continue;
        };
        let (min, max) = match params.get(m.param_id.as_ref()) {
            Some(p) => (p.spec.min, p.spec.max),
            None => continue,
        };
        let raw = m.source.feature.extract(features);
        let shape = m.shape;
        let out_norm = shape.apply(raw, dt_s, &mut m.smoothed, &mut m.prev_raw);
        if let Some(p) = params.get_mut(m.param_id.as_ref()) {
            if p.spec.is_trigger {
                // §8 D5b: a fire-button target wants a monotonic count, not a
                // level — edge-detect the shaped signal (rising through
                // mid-range) instead of overwriting continuously. Downstream
                // `last_count`-style consumers edge-detect the written value
                // unchanged.
                if m.trigger_edge.advance(out_norm, 0.5) {
                    m.fire_count = m.fire_count.wrapping_add(1);
                }
                p.value = p.base + m.fire_count as f32;
            } else {
                p.value = min + (max - min) * out_norm;
            }
            wrote = true;
        }
    }
    wrote
}

/// §8 D2/D4: one instance's own `audio_trigger` config fired this tick — the
/// caller sums fires into the owning layer's/master's `audio_count` (P2,
/// renderer-owned). Pure edge detection over the config's own send/band, same
/// shared `TransientEdge` the clip-trigger evaluator uses. Skipped when the
/// mode doesn't want the audio path — see `TriggerFireMode::wants_transient`.
fn evaluate_instance_trigger(
    fx: &mut PresetInstance,
    by_send: &HashMap<AudioSendId, SendFeatures>,
) -> bool {
    let Some(cfg) = fx.audio_trigger.as_mut() else {
        return false;
    };
    if !cfg.enabled || !cfg.mode.wants_transient() {
        return false;
    }
    let Some(features) = by_send.get(&cfg.source.send_id) else {
        return false;
    };
    let level = cfg.source.feature.extract(features);
    let threshold = cfg.threshold();
    cfg.edge.advance(level, threshold)
}

/// One instance's audio-trigger fire this tick (§8 D1), for the renderer (P2)
/// to fold into its `audio_count`. `layer_id` is `None` for a master-chain
/// instance (D5: "master/global chains have no layer... audio fires still
/// work") — the renderer keys a master-scoped counter off that sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerPulse {
    pub layer_id: Option<manifold_core::id::LayerId>,
}

/// Walk master effects, layer effects, and generator instances — the same
/// instance set `evaluate_all_audio_mods` walks — evaluating each instance's
/// own `audio_trigger` config (§8 D2) and collecting a pulse per fire. Pure:
/// only mutates each config's runtime `TransientEdge`. Self-contained (builds
/// its own send→features map, mirroring `evaluate_all_audio_mods`) so it can
/// run as its own step in `evaluate_modulation`.
pub fn evaluate_all_param_triggers(
    project: &mut Project,
    snapshot: &AudioFeatureSnapshot,
) -> Vec<TriggerPulse> {
    let mut pulses = Vec::new();
    if snapshot.is_empty() || project.audio_setup.sends.is_empty() {
        return pulses;
    }
    let mut by_send: HashMap<AudioSendId, SendFeatures> = HashMap::new();
    for (i, send) in project.audio_setup.sends.iter().enumerate() {
        if let Some(f) = snapshot.get(i) {
            by_send.insert(send.id.clone(), *f);
        }
    }
    if by_send.is_empty() {
        return pulses;
    }

    for fx in project.settings.master_effects.iter_mut() {
        if evaluate_instance_trigger(fx, &by_send) {
            pulses.push(TriggerPulse { layer_id: None });
        }
    }
    for layer in project.timeline.layers.iter_mut() {
        let layer_id = layer.layer_id.clone();
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if evaluate_instance_trigger(fx, &by_send) {
                    pulses.push(TriggerPulse { layer_id: Some(layer_id.clone()) });
                }
            }
        }
        if let Some(gp) = layer.gen_params_mut()
            && evaluate_instance_trigger(gp, &by_send)
        {
            pulses.push(TriggerPulse { layer_id: Some(layer_id) });
        }
    }
    pulses
}

/// BUG-051 fix (§8 D4): drop every audio-trigger edge-detector's armed state
/// back to armed. Call on transport stop / project reset so a stale "fired,
/// not yet re-armed" flag can't suppress the first onset next time. Walks the
/// same instance set as [`evaluate_all_param_triggers`], clearing both the
/// per-instance `audio_trigger.edge` (D2) and every `is_trigger`-target
/// `ParameterAudioMod.trigger_edge` (D5b) — the two new edge-state holders
/// this wave introduced.
pub fn clear_all_trigger_edges(project: &mut Project) {
    fn clear_instance(fx: &mut PresetInstance) {
        if let Some(cfg) = fx.audio_trigger.as_mut() {
            cfg.edge.clear();
        }
        if let Some(mods) = fx.audio_mods.as_mut() {
            for m in mods.iter_mut() {
                m.trigger_edge.clear();
            }
        }
    }

    for fx in project.settings.master_effects.iter_mut() {
        clear_instance(fx);
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                clear_instance(fx);
            }
        }
        if let Some(gp) = layer.gen_params_mut() {
            clear_instance(gp);
        }
    }
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
    /// registry defaults (populated `params` manifest, kind = Generator).
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
            (fx.params.get("amount").unwrap().value - 1.0).abs() < 1e-6,
            "amount driven from base 0.0 to max 1.0, got {}",
            fx.params.get("amount").unwrap().value
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
            (fx.params.get("amount").unwrap().value - 0.5).abs() < 1e-6,
            "amount driven to half-range 0.5, got {}",
            fx.params.get("amount").unwrap().value
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
        let v_half = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap()
            .value;
        assert!(
            (v_half - 0.5).abs() < 1e-5,
            "half-decay drives amount to ~0.5, got {v_half}"
        );

        // Reset base, then past the decay window → level 0 → no change.
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .params
            .get_mut("amount")
            .unwrap()
            .value = 0.0;
        let past = (DEFAULT_ENVELOPE_DECAY_BEATS + 0.5) as f64;
        assert!(!evaluate_all_envelopes(&mut project, &[(Beats(past), Beats(8.0))]));
        let v_past = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap()
            .value;
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
        assert_eq!(fx.params.get("amount").unwrap().value, 0.0, "param untouched");
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
        assert_eq!(fx.params.get("amount").unwrap().value, 0.0);
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
        assert_eq!(fx.params.get("amount").unwrap().value, 0.0);
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
            effects[0].params.get("amount").unwrap().value, 0.0,
            "the effect WITHOUT an envelope is untouched"
        );
        assert!(
            (effects[1].params.get("amount").unwrap().value - 1.0).abs() < 1e-6,
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
            (gp.params.get("speed").unwrap().value - 10.0).abs() < 1e-6,
            "speed driven from 0 to max 10, got {}",
            gp.params.get("speed").unwrap().value
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
        assert_eq!(gp.params.get("speed").unwrap().value, 0.0);
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
            (fx.params.get("amount").unwrap().value - 1.0).abs() < 1e-6,
            "amount driven to 1.0, got {}",
            fx.params.get("amount").unwrap().value
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
        assert_eq!(fx.params.get("amount").unwrap().value, 0.0, "param untouched");
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
        assert_eq!(fx.params.get("amount").unwrap().value, 0.0);
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
        assert_eq!(fx.params.get("amount").unwrap().value, 0.0);
    }

    /// Recalibrate a STOCK param's range in place on the instance's own
    /// manifest entry — exactly what the chevron popover writes (D6:
    /// calibration edits `spec.min`/`spec.max` in place and sets
    /// `Param::calibrated`; there is no separate graph/`preset_metadata`
    /// overlay to resolve through anymore — `p.spec` on the manifest entry
    /// IS the effective range).
    fn override_stock_param_range(inst: &mut PresetInstance, param_id: &str, min: f32, max: f32) {
        let p = inst.params.get_mut(param_id).expect("param must exist on the manifest");
        p.spec.min = min;
        p.spec.max = max;
        p.calibrated = true;
    }

    #[test]
    fn audio_mod_respects_recalibrated_range_override() {
        // Narrow `amount`'s range to 0..0.5 on this instance (what the chevron
        // popover writes, in place, onto the manifest entry's `spec`). A
        // full-signal audio mod must cap at the override max 0.5, not the
        // catalog max 1.0 — the recalibrated range IS the effective range, so
        // a recalibrated slider bounds the modulator too.
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
            (fx.params.get("amount").unwrap().value - 0.5).abs() < 1e-6,
            "full signal caps at the recalibrated max 0.5, got {}",
            fx.params.get("amount").unwrap().value,
        );
    }

    // ── §8 param triggers ────────────────────────────────────────────────

    use manifold_core::audio_mod::AudioModSource;
    use manifold_core::audio_trigger::{AudioTriggerMod, TriggerFireMode};

    const TEST_TRIGGER_FX: PresetTypeId = PresetTypeId::new("TestTriggerFx");

    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::new("TestTriggerFx"),
            display_name: "Test Trigger Fx",
            category: "Test",
            available: true,
            osc_prefix: "testTriggerFx",
            legacy_discriminant: None,
            params: &[ParamSpec::trigger("fire", "Fire", "")],
        }
    }

    fn audio_trigger_cfg(
        send_id: &AudioSendId,
        sensitivity: f32,
        mode: TriggerFireMode,
    ) -> AudioTriggerMod {
        AudioTriggerMod {
            enabled: true,
            source: AudioModSource {
                send_id: send_id.clone(),
                feature: AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
            },
            sensitivity,
            mode,
            edge: Default::default(),
        }
    }

    /// A snapshot with one send whose Full-band transient reads `level`.
    fn snapshot_full_transient(level: f32) -> AudioFeatureSnapshot {
        let mut s = AudioFeatureSnapshot { sends: vec![SendFeatures::default()] };
        s.sends[0].bands[AudioBand::Full.index()].transients = level;
        s
    }

    #[test]
    fn param_trigger_fires_on_generator_and_produces_layer_pulse() {
        let mut project = project_with(generator_layer());
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        let layer_id = project.timeline.layers[0].layer_id.clone();
        project.timeline.layers[0].gen_params_mut().unwrap().audio_trigger =
            Some(audio_trigger_cfg(&send_id, 0.5, TriggerFireMode::Both));

        let hot = snapshot_full_transient(0.99);
        let pulses = evaluate_all_param_triggers(&mut project, &hot);
        assert_eq!(pulses, vec![TriggerPulse { layer_id: Some(layer_id) }]);
    }

    #[test]
    fn param_trigger_mode_clip_edge_ignores_transients() {
        let mut project = project_with(generator_layer());
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        project.timeline.layers[0].gen_params_mut().unwrap().audio_trigger =
            Some(audio_trigger_cfg(&send_id, 0.5, TriggerFireMode::ClipEdge));

        let hot = snapshot_full_transient(0.99);
        assert!(
            evaluate_all_param_triggers(&mut project, &hot).is_empty(),
            "ClipEdge mode (default) must not react to audio"
        );
    }

    #[test]
    fn param_trigger_master_effect_pulse_has_no_layer() {
        let mut project = Project::default();
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        let mut fx = create_default(&TEST_FX);
        fx.audio_trigger = Some(audio_trigger_cfg(&send_id, 0.5, TriggerFireMode::Transient));
        project.settings.master_effects.push(fx);

        let hot = snapshot_full_transient(0.99);
        let pulses = evaluate_all_param_triggers(&mut project, &hot);
        assert_eq!(pulses, vec![TriggerPulse { layer_id: None }]);
    }

    #[test]
    fn param_trigger_edge_rearms_between_fires() {
        let mut project = project_with(generator_layer());
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        project.timeline.layers[0].gen_params_mut().unwrap().audio_trigger =
            Some(audio_trigger_cfg(&send_id, 0.5, TriggerFireMode::Transient));

        let hot = snapshot_full_transient(0.99);
        assert_eq!(evaluate_all_param_triggers(&mut project, &hot).len(), 1);
        assert_eq!(
            evaluate_all_param_triggers(&mut project, &hot).len(),
            0,
            "still hot, no re-fire"
        );
    }

    #[test]
    fn clear_all_trigger_edges_rearms_generator_edge() {
        let mut project = project_with(generator_layer());
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        project.timeline.layers[0].gen_params_mut().unwrap().audio_trigger =
            Some(audio_trigger_cfg(&send_id, 0.5, TriggerFireMode::Transient));

        let hot = snapshot_full_transient(0.99);
        assert_eq!(evaluate_all_param_triggers(&mut project, &hot).len(), 1);
        assert_eq!(evaluate_all_param_triggers(&mut project, &hot).len(), 0); // disarmed

        clear_all_trigger_edges(&mut project);
        assert_eq!(
            evaluate_all_param_triggers(&mut project, &hot).len(),
            1,
            "clear() re-arms so the held-hot signal fires again (BUG-051)"
        );
    }

    #[test]
    fn is_trigger_audio_mod_writes_monotonic_count_not_continuous_value() {
        let (mut project, send_id) = project_with_audio_send();
        project.timeline.layers[0].effects = Some(vec![create_default(&TEST_TRIGGER_FX)]);
        let mut m = ParameterAudioMod::new(
            "fire".into(),
            send_id,
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low),
        );
        m.shape = AudioModShape { attack_ms: 0.0, release_ms: 0.0, ..Default::default() };
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods_mut()
            .push(m);

        let hot = snapshot_low(1.0);
        let cold = snapshot_low(0.0);

        assert!(evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016)));
        let value = |p: &Project| p.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("fire")
            .unwrap()
            .value;
        assert_eq!(value(&project), 1.0, "first rising edge bumps the count to 1");

        // Still hot: no re-fire, count holds at 1.
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016));
        assert_eq!(value(&project), 1.0);

        // Decay below the rearm floor, then fire again: count bumps to 2.
        evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016));
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016));
        assert_eq!(value(&project), 2.0);
    }
}
