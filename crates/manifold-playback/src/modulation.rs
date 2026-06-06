//! Modulation pipeline — per-frame driver (LFO) and envelope (ADSR) evaluation.
//!
//! Port of C# DriverController + ParameterDriverManager + EnvelopeEvaluator.
//!
//! Execution order each frame (after SyncClipsToTime, before compositor):
//!   1. reset_all_effectives(project)        — base → effective
//!   2. evaluate_all_drivers(project, beat)   — LFO → effective
//!   3. evaluate_all_envelopes(project, beat) — ADSR → effective (additive)
//!   4. evaluate_gen_param_envelopes(project, beat) — gen ADSR → effective (additive)
//!   5. If any_dirty → mark compositor dirty

use manifold_core::Beats;
use manifold_core::effects::{PresetInstance, EnvelopeMode, ParamEnvelope, ParameterDriver};
use manifold_core::preset_definition_registry;
use manifold_core::project::Project;
use manifold_core::types::LayerType;

// ── Random envelope helpers ─────────────────────────────────────────────────

/// Non-deterministic float in [0, 1). Used for random walk/jump envelopes.
fn random_unit() -> f32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos()
        .hash(&mut hasher);
    (hasher.finish() & 0x00FF_FFFFu64) as f32 / 16_777_216.0
}

/// Compute the next random walk/jump value on a rising edge.
/// `walk_value`: current position [0, 1].
/// `step_size`: normalized step (from target_normalized).
/// `random_jump`: if true, jump to a fully random value instead of walking.
/// `whole_numbers`: if true, quantize to discrete steps.
/// `min`/`max`: param range (needed for discrete quantization).
/// `range_min`/`range_max`: normalized range constraints [0, 1].
/// Returns the new walk_value in [range_min, range_max].
#[allow(clippy::too_many_arguments)]
fn compute_random_step(
    walk_value: f32,
    step_size: f32,
    random_jump: bool,
    whole_numbers: bool,
    min: f32,
    max: f32,
    range_min: f32,
    range_max: f32,
) -> f32 {
    if random_jump {
        let raw = random_unit();
        if whole_numbers && (max - min) > 0.0 {
            let steps = (max - min).round() as i32;
            if steps > 0 {
                let lo = (range_min * steps as f32).round() as i32;
                let hi = (range_max * steps as f32).round() as i32;
                let span = (hi - lo).max(0);
                let idx = lo + (raw * (span + 1) as f32).floor() as i32;
                let idx = idx.clamp(lo, hi);
                return idx as f32 / steps as f32;
            }
        }
        return range_min + raw * (range_max - range_min);
    }

    // Random walk: step up or down by step_size, clamp at range boundaries.
    let up = random_unit() < 0.5;

    if whole_numbers && (max - min) > 0.0 {
        let steps = (max - min).round() as i32;
        if steps > 0 {
            let step_units = ((step_size * steps as f32).round() as i32).max(1);
            let current_idx = (walk_value * steps as f32).round() as i32;
            let lo = (range_min * steps as f32).round() as i32;
            let hi = (range_max * steps as f32).round() as i32;
            let new_idx = if up {
                (current_idx + step_units).min(hi)
            } else {
                (current_idx - step_units).max(lo)
            };
            return new_idx as f32 / steps as f32;
        }
    }

    // Continuous walk — clamp at [range_min, range_max]
    if up {
        (walk_value + step_size).min(range_max)
    } else {
        (walk_value - step_size).max(range_min)
    }
}

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
    let mut normalized = ParameterDriver::evaluate(
        current_beat,
        driver.beat_division,
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

/// Pure core of a Random-mode envelope's per-frame evaluation.
///
/// Returns `Some((new_walk, held_value))` when the envelope produces a value
/// this frame; `None` when the walk is still uninitialized (`walk_value < 0`)
/// and no trigger fired — the caller holds and only refreshes its edge-detection
/// bookkeeping.
#[allow(clippy::too_many_arguments)]
fn random_envelope_value(
    trigger: bool,
    walk_value: f32,
    random_jump: bool,
    whole: bool,
    min: f32,
    max: f32,
    range_min: f32,
    range_max: f32,
) -> Option<(f32, f32)> {
    let new_walk = if trigger {
        if walk_value < 0.0 {
            compute_random_step(0.5, 1.0, true, whole, min, max, range_min, range_max)
        } else {
            compute_random_step(
                walk_value,
                0.15,
                random_jump,
                whole,
                min,
                max,
                range_min,
                range_max,
            )
        }
    } else if walk_value < 0.0 {
        return None;
    } else {
        walk_value
    };
    let new_walk = new_walk.clamp(range_min, range_max);

    let held = min + (max - min) * new_walk;
    let held = if whole {
        held.round().clamp(min, max)
    } else {
        held.clamp(min, max)
    };
    Some((new_walk, held))
}

/// Apply an ADSR envelope's additive offset to a single param slot value.
/// Returns true if the value changed.
fn apply_adsr_offset(value: &mut f32, min: f32, max: f32, target_norm: f32, adsr: f32) -> bool {
    let current = *value;
    let target = min + (max - min) * target_norm.clamp(0.0, 1.0);
    let offset = (target - current) * adsr;
    let final_value = (current + offset).clamp(min, max);
    if (final_value - current).abs() > f32::EPSILON {
        *value = final_value;
        true
    } else {
        false
    }
}

/// Refresh an envelope's rising-edge bookkeeping without applying a value —
/// the path taken when an envelope is disabled or its target param can't be
/// resolved this frame.
fn refresh_env_clip_active(inst: &mut PresetInstance, ei: usize, clip_active: bool) {
    if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(ei)) {
        env.was_clip_active = clip_active;
    }
}

/// Apply every envelope (ADSR + Random) carried by one instance against the
/// active-clip timing of the container it lives in. Returns true if any
/// envelope wrote a value this frame.
///
/// Since envelope-home unification this is the single envelope walk for both
/// kinds: an envelope lives on its owning `PresetInstance`, and
/// `resolve_param_in` resolves a param id against either an effect or a
/// generator definition — so the two formerly-parallel blocks
/// (`evaluate_all_envelopes` for effects, `evaluate_gen_param_envelopes` for
/// generators) collapse to a caller that locates the def + timing and hands
/// off here. Byte-for-byte the prior per-envelope arithmetic — extracted, not
/// changed.
fn apply_instance_envelopes(
    inst: &mut PresetInstance,
    def: &manifold_core::preset_def::PresetDef,
    active_elapsed: Beats,
    active_duration: Beats,
) -> bool {
    let clip_active = active_elapsed >= Beats::ZERO;
    let env_count = inst.envelopes.as_ref().map_or(0, |e| e.len());
    let mut any_modulated = false;

    // Index-based iteration to avoid cloning the envelope vec (Phase 9C fix).
    for ei in 0..env_count {
        let (
            enabled,
            param_id,
            attack,
            decay,
            sustain,
            release,
            target_norm,
            mode,
            random_jump,
            walk_value,
            was_active,
            last_elapsed,
            range_min,
            range_max,
        ) = {
            let env = &inst.envelopes.as_ref().unwrap()[ei];
            (
                env.enabled,
                env.param_id.clone(),
                env.attack_beats,
                env.decay_beats,
                env.sustain_level,
                env.release_beats,
                env.target_normalized,
                env.mode,
                env.random_jump,
                env.walk_value,
                env.was_clip_active,
                env.last_elapsed,
                env.range_min,
                env.range_max,
            )
        };

        if !enabled {
            refresh_env_clip_active(inst, ei, clip_active);
            continue;
        }

        let Some(resolved) =
            manifold_core::effects::resolve_param_in(def, inst, param_id.as_ref())
        else {
            refresh_env_clip_active(inst, ei, clip_active);
            continue;
        };
        if resolved.idx >= inst.param_values.len() {
            refresh_env_clip_active(inst, ei, clip_active);
            continue;
        }
        let idx = resolved.idx;
        let (min, max) = (resolved.min, resolved.max);
        let whole = resolved.whole_numbers;

        // ── Random mode: sample & hold ─────────────────────────
        // Trigger conditions (same events that restart an ADSR envelope):
        //   1. Clip becomes active after being inactive (!was_active)
        //   2. Elapsed decreases — new sequential clip or loop restart
        //   3. First evaluation after mode switch (last_elapsed sentinel)
        if mode == EnvelopeMode::Random {
            let elapsed_f = active_elapsed.as_f32();
            let trigger =
                clip_active && (last_elapsed < 0.0 || elapsed_f < last_elapsed || !was_active);

            match random_envelope_value(
                trigger, walk_value, random_jump, whole, min, max, range_min, range_max,
            ) {
                None => {
                    if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(ei)) {
                        env.was_clip_active = clip_active;
                        env.last_elapsed = elapsed_f;
                    }
                }
                Some((new_walk, held)) => {
                    if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(ei)) {
                        env.walk_value = new_walk;
                        env.current_level = new_walk;
                        env.was_clip_active = clip_active;
                        env.last_elapsed = elapsed_f;
                    }
                    inst.param_values[idx].value = held;
                    any_modulated = true;
                }
            }
            continue;
        }

        // ── ADSR mode ────────────────────────────────────────────
        if !clip_active {
            if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(ei)) {
                env.was_clip_active = false;
            }
            continue;
        }

        let adsr_value = ParamEnvelope::calculate_adsr(
            active_elapsed,
            active_duration,
            attack,
            decay,
            sustain,
            release,
        );

        if let Some(env) = inst.envelopes.as_mut().and_then(|e| e.get_mut(ei)) {
            env.current_level = adsr_value;
            env.was_clip_active = clip_active;
        }

        if apply_adsr_offset(
            &mut inst.param_values[idx].value,
            min,
            max,
            target_norm,
            adsr_value,
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
        if evaluate_effect_drivers(fx, current_beat) {
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
                if evaluate_effect_drivers(fx, current_beat) {
                    any_driven = true;
                }
            }
        }

        // Generator param drivers
        if layer.layer_type == LayerType::Generator
            && let Some(gp) = layer.gen_params_mut()
        {
            let gen_type = gp.generator_type();
            let gen_def = match preset_definition_registry::generator::try_get(gen_type) {
                Some(d) => d,
                None => continue,
            };
            let gen_defs = &gen_def.param_defs;

            if let Some(drivers) = &gp.drivers {
                // Collect driver evaluation results to avoid borrow conflict
                let results: Vec<(usize, f32)> = drivers
                    .iter()
                    .filter(|d| d.enabled && !d.is_paused_by_user)
                    .filter_map(|driver| {
                        let &idx = gen_def.id_to_index.get(driver.param_id.as_ref())?;
                        if idx >= gen_defs.len() {
                            return None;
                        }
                        let pd = &gen_defs[idx];
                        let value = driver_target_value(driver, current_beat, pd.min, pd.max);
                        Some((idx, value))
                    })
                    .collect();

                if !results.is_empty() {
                    any_driven = true;
                    for (idx, value) in results {
                        if idx < gp.param_values.len() {
                            gp.param_values[idx].value = value;
                        }
                    }
                }
            }
        }
    }

    any_driven
}

/// Evaluate all drivers on a single PresetInstance. Returns true if any driver was active.
/// Port of C# ParameterDriverManager.EvaluateEffectDrivers().
fn evaluate_effect_drivers(fx: &mut PresetInstance, current_beat: Beats) -> bool {
    if !fx.enabled {
        return false;
    }
    let drivers = match &fx.drivers {
        Some(d) if !d.is_empty() => d,
        _ => return false,
    };

    let effect_def = match preset_definition_registry::effect::try_get(fx.effect_type()) {
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
        let (active_elapsed, active_duration) = active_clip_timing
            .get(li)
            .copied()
            .unwrap_or((Beats(-1.0), Beats::ZERO));

        let Some(effects) = layer.effects.as_mut() else {
            continue;
        };
        for fx in effects.iter_mut() {
            if !fx.enabled {
                continue;
            }
            let Some(def) = preset_definition_registry::effect::try_get(fx.effect_type()) else {
                continue;
            };
            if apply_instance_envelopes(fx, &def, active_elapsed, active_duration) {
                any_modulated = true;
            }
        }
    }

    any_modulated
}

// =====================================================================
// Phase 4: Evaluate generator param envelopes
// Port of C# EnvelopeEvaluator.EvaluateGenParamEnvelopes()
// =====================================================================

/// Evaluate envelopes (ADSR and Random) on generator layer parameters.
/// Returns true if any envelope was active (compositor should be marked dirty).
///
/// Generator envelopes already lived on the instance (`gp.envelopes`); this is
/// now the same walk the effect pass uses — see [`apply_instance_envelopes`].
pub fn evaluate_gen_param_envelopes(
    project: &mut Project,
    active_clip_timing: &[(Beats, Beats)],
) -> bool {
    let mut any_modulated = false;

    for (li, layer) in project.timeline.layers.iter_mut().enumerate() {
        if layer.layer_type != LayerType::Generator {
            continue;
        }

        let (active_elapsed, active_duration) = active_clip_timing
            .get(li)
            .copied()
            .unwrap_or((Beats(-1.0), Beats::ZERO));

        let Some(gp) = layer.gen_params_mut() else {
            continue;
        };
        let Some(def) = preset_definition_registry::generator::try_get(gp.generator_type()) else {
            continue;
        };
        if apply_instance_envelopes(gp, &def, active_elapsed, active_duration) {
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
    timing_scratch: &mut Vec<(Beats, Beats)>,
) -> bool {
    // Phase 1: Reset all effective values to base
    reset_all_effectives(project);

    // Phase 2: Evaluate LFO drivers
    let any_driven = evaluate_all_drivers(project, current_beat);

    // Pre-compute per-layer active clip timing for envelope phases.
    // Avoids O(total_clips) scan in each envelope function.
    compute_active_clip_timing(&project.timeline.layers, current_beat, timing_scratch);

    // Phase 3: Evaluate clip/layer ADSR envelopes (additive on top of drivers)
    let any_enveloped = evaluate_all_envelopes(project, timing_scratch);

    // Phase 4: Evaluate generator param ADSR envelopes
    let any_gen_enveloped = evaluate_gen_param_envelopes(project, timing_scratch);

    any_driven || any_enveloped || any_gen_enveloped
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
// These pin the *current* behavior of `evaluate_all_envelopes` (effect
// targets) and `evaluate_gen_param_envelopes` (generator params) before the
// envelope-home unification moves effect envelopes off `layer.envelopes`
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
    use manifold_core::effects::{EnvelopeMode, ParamEnvelope};
    use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};
    use manifold_core::layer::Layer;
    use manifold_core::preset_definition_registry::effect;
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
            string_params: &[],
        }
    }

    /// A video layer carrying one default `TestEnvFx` effect instance.
    fn layer_with_one_effect() -> Layer {
        let mut layer = Layer::new_video("FxLayer".into(), 0);
        layer.effects = Some(vec![effect::create_default(&TEST_FX)]);
        layer
    }

    /// A generator layer whose `gen_params` is initialized to `TestEnvGen`
    /// registry defaults (populated `param_values`, kind = Generator).
    fn generator_layer() -> Layer {
        let mut layer = Layer::new_generator("GenLayer".into(), TEST_GEN.clone(), 0);
        layer.gen_params_or_init().init_defaults_for_type(TEST_GEN.clone());
        layer
    }

    /// ADSR sustain=1, attack=decay=release=0 → calculate_adsr returns 1.0 for
    /// any in-clip position, so this is a pure "full offset" envelope.
    fn adsr_full(param: &'static str) -> ParamEnvelope {
        let mut env = ParamEnvelope::new(param);
        env.sustain_level = 1.0;
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

    // ── effect ADSR ──────────────────────────────────────────────────────

    #[test]
    fn effect_adsr_envelope_applies_full_offset_to_targeted_param() {
        let layer = effect_layer_with_env(adsr_full("amount"));
        let mut project = project_with(layer);

        // Active clip: elapsed 2 beats within an 8-beat clip.
        let timing = vec![(Beats(2.0), Beats(8.0))];
        let modulated = evaluate_all_envelopes(&mut project, &timing);

        assert!(modulated, "an in-clip full-offset envelope reports modulation");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].value - 1.0).abs() < 1e-6,
            "amount driven from base 0.0 to max 1.0, got {}",
            fx.param_values[0].value
        );
    }

    #[test]
    fn effect_adsr_partial_target_normalized() {
        let mut env = adsr_full("amount");
        env.target_normalized = 0.5; // target = min + (max-min)*0.5 = 0.5
        let layer = effect_layer_with_env(env);
        let mut project = project_with(layer);

        let timing = vec![(Beats(2.0), Beats(8.0))];
        assert!(evaluate_all_envelopes(&mut project, &timing));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].value - 0.5).abs() < 1e-6,
            "amount driven to half-range 0.5, got {}",
            fx.param_values[0].value
        );
    }

    #[test]
    fn effect_adsr_no_change_when_clip_inactive() {
        let layer = effect_layer_with_env(adsr_full("amount"));
        let mut project = project_with(layer);

        // No active clip on this layer (sentinel elapsed -1).
        let timing = vec![(Beats(-1.0), Beats::ZERO)];
        let modulated = evaluate_all_envelopes(&mut project, &timing);

        assert!(!modulated, "no active clip => no modulation");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0, "param untouched");
        assert!(
            !fx.envelopes.as_ref().unwrap()[0].was_clip_active,
            "rising-edge bookkeeping cleared when clip inactive"
        );
    }

    #[test]
    fn effect_disabled_envelope_is_noop() {
        let mut env = adsr_full("amount");
        env.enabled = false;
        let layer = effect_layer_with_env(env);
        let mut project = project_with(layer);

        let timing = vec![(Beats(2.0), Beats(8.0))];
        assert!(!evaluate_all_envelopes(&mut project, &timing));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].value, 0.0);
    }

    #[test]
    fn envelope_on_disabled_effect_is_noop() {
        // An envelope riding on a disabled effect doesn't modulate (matches the
        // prior find(enabled) gate, now expressed as "skip disabled effects").
        let mut layer = effect_layer_with_env(adsr_full("amount"));
        layer.effects.as_mut().unwrap()[0].enabled = false;
        let mut project = project_with(layer);

        let timing = vec![(Beats(2.0), Beats(8.0))];
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
        let mut fx_a = effect::create_default(&TEST_FX);
        let mut fx_b = effect::create_default(&TEST_FX);
        // Only the SECOND same-type effect carries an envelope.
        fx_a.envelopes = None;
        fx_b.envelopes = Some(vec![adsr_full("amount")]);
        layer.effects = Some(vec![fx_a, fx_b]);
        let mut project = project_with(layer);

        let timing = vec![(Beats(2.0), Beats(8.0))];
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

    // ── effect Random ────────────────────────────────────────────────────

    #[test]
    fn effect_random_envelope_jumps_within_normalized_range() {
        let mut env = ParamEnvelope::new("amount");
        env.mode = EnvelopeMode::Random;
        env.random_jump = true;
        env.range_min = 0.25;
        env.range_max = 0.75;
        let layer = effect_layer_with_env(env);
        let mut project = project_with(layer);

        // Clip just became active (was_clip_active false, last_elapsed -1) → trigger.
        let timing = vec![(Beats(0.0), Beats(8.0))];
        let modulated = evaluate_all_envelopes(&mut project, &timing);

        assert!(modulated, "random trigger reports modulation");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        // min + (max-min)*range, with min=0,max=1 → held in [0.25, 0.75].
        assert!(
            (0.25..=0.75).contains(&fx.param_values[0].value),
            "random jump held within range, got {}",
            fx.param_values[0].value
        );
        let env = &fx.envelopes.as_ref().unwrap()[0];
        assert!(
            (0.25..=0.75).contains(&env.walk_value),
            "walk_value initialized within range, got {}",
            env.walk_value
        );
    }

    // ── generator ADSR ───────────────────────────────────────────────────

    #[test]
    fn generator_adsr_envelope_applies_full_offset() {
        let mut layer = generator_layer();
        {
            let gp = layer.gen_params_or_init();
            let mut env = ParamEnvelope::new("speed");
            env.sustain_level = 1.0;
            env.target_normalized = 1.0;
            gp.envelopes = Some(vec![env]);
        }
        let mut project = project_with(layer);

        let timing = vec![(Beats(2.0), Beats(8.0))];
        let modulated = evaluate_gen_param_envelopes(&mut project, &timing);

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
            env.sustain_level = 1.0;
            gp.envelopes = Some(vec![env]);
        }
        let mut project = project_with(layer);

        let timing = vec![(Beats(2.0), Beats(8.0))];
        assert!(!evaluate_gen_param_envelopes(&mut project, &timing));
        let gp = project.timeline.layers[0].gen_params().unwrap();
        assert_eq!(gp.param_values[0].value, 0.0);
    }

    #[test]
    fn generator_envelope_only_runs_on_generator_layers() {
        // An effect layer carrying a gen-style envelope on its (absent)
        // gen_params is never reached by the generator evaluator.
        let layer = layer_with_one_effect();
        let mut project = project_with(layer);
        let timing = vec![(Beats(2.0), Beats(8.0))];
        assert!(!evaluate_gen_param_envelopes(&mut project, &timing));
    }
}
