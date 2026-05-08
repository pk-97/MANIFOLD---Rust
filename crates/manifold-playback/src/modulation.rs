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
use manifold_core::effects::{EffectInstance, EnvelopeMode, ParamEnvelope, ParameterDriver};
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{effect_definition_registry, generator_definition_registry};

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
            let gen_def = match generator_definition_registry::try_get(gen_type) {
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
                        let (min, max) = (pd.min, pd.max);

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
                        let value = lo + (hi - lo) * normalized;

                        Some((idx, value))
                    })
                    .collect();

                if !results.is_empty() {
                    any_driven = true;
                    for (idx, value) in results {
                        if idx < gp.param_values.len() {
                            gp.param_values[idx] = value;
                        }
                    }
                }
            }
        }
    }

    any_driven
}

/// Evaluate all drivers on a single EffectInstance. Returns true if any driver was active.
/// Port of C# ParameterDriverManager.EvaluateEffectDrivers().
fn evaluate_effect_drivers(fx: &mut EffectInstance, current_beat: Beats) -> bool {
    if !fx.enabled {
        return false;
    }
    let drivers = match &fx.drivers {
        Some(d) if !d.is_empty() => d,
        _ => return false,
    };

    let effect_def = match effect_definition_registry::try_get(fx.effect_type()) {
        Some(d) => d,
        None => return false,
    };
    let effect_defs = &effect_def.param_defs;
    let mut any_driven = false;

    // Collect results to avoid borrow conflict between drivers and param_values
    let results: Vec<(usize, f32)> = drivers
        .iter()
        .filter(|d| d.enabled && !d.is_paused_by_user)
        .filter_map(|driver| {
            let &idx = effect_def.id_to_index.get(driver.param_id.as_ref())?;
            if idx >= effect_defs.len() {
                return None;
            }
            let (min, max) = (effect_defs[idx].min, effect_defs[idx].max);

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
            let value = lo + (hi - lo) * normalized;

            Some((idx, value))
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

/// Evaluate all layer envelopes (ADSR and Random modes).
/// Returns true if any envelope was active (compositor should be marked dirty).
pub fn evaluate_all_envelopes(
    project: &mut Project,
    active_clip_timing: &[(Beats, Beats)],
) -> bool {
    let mut any_modulated = false;

    for (li, layer) in project.timeline.layers.iter_mut().enumerate() {
        // Envelopes run even on muted layers (mute = compositor only).
        let (active_elapsed, active_duration) = active_clip_timing
            .get(li)
            .copied()
            .unwrap_or((Beats(-1.0), Beats::ZERO));

        let clip_active = active_elapsed >= Beats::ZERO;

        // Evaluate layer envelopes (use timing from first active clip).
        // Uses index-based iteration to avoid cloning (Phase 9C fix).
        let layer_env_count = layer.envelopes.as_ref().map_or(0, |e| e.len());
        if layer_env_count == 0 {
            continue;
        }

        if layer.effects.is_none() {
            // Still update was_clip_active for rising edge detection
            for ei in 0..layer_env_count {
                if let Some(envs) = &mut layer.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = clip_active;
                }
            }
            continue;
        }

        for ei in 0..layer_env_count {
            let (
                enabled,
                target_effect_type,
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
                let env = &layer.envelopes.as_ref().unwrap()[ei];
                (
                    env.enabled,
                    env.target_effect_type.clone(),
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
                if let Some(envs) = &mut layer.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = clip_active;
                }
                continue;
            }

            // ── Random mode: sample & hold ─────────────────────────
            // Trigger conditions (same events that restart an ADSR envelope):
            //   1. Clip becomes active after being inactive (!was_active)
            //   2. Elapsed decreases — new clip started on the same layer
            //      (sequential clips each reset elapsed to 0) or loop restart
            //   3. First evaluation after mode switch (last_elapsed sentinel)
            if mode == EnvelopeMode::Random {
                let elapsed_f = active_elapsed.as_f32();
                let trigger = clip_active
                    && (last_elapsed < 0.0 || elapsed_f < last_elapsed || !was_active);

                let layer_effects = match &mut layer.effects {
                    Some(effects) => effects,
                    None => {
                        if let Some(envs) = &mut layer.envelopes
                            && let Some(env) = envs.get_mut(ei)
                        {
                            env.was_clip_active = clip_active;
                        }
                        continue;
                    }
                };
                let target_fx = layer_effects
                    .iter_mut()
                    .find(|f| f.effect_type() == &target_effect_type && f.enabled);
                let fx = match target_fx {
                    Some(f) => f,
                    None => {
                        if let Some(envs) = &mut layer.envelopes
                            && let Some(env) = envs.get_mut(ei)
                        {
                            env.was_clip_active = clip_active;
                        }
                        continue;
                    }
                };
                let effect_def =
                    match effect_definition_registry::try_get(fx.effect_type()) {
                        Some(d) => d,
                        None => continue,
                    };
                let Some(&idx) = effect_def.id_to_index.get(param_id.as_ref()) else {
                    if let Some(envs) = &mut layer.envelopes
                        && let Some(env) = envs.get_mut(ei)
                    {
                        env.was_clip_active = clip_active;
                    }
                    continue;
                };
                if idx >= effect_def.param_defs.len() || idx >= fx.param_values.len() {
                    if let Some(envs) = &mut layer.envelopes
                        && let Some(env) = envs.get_mut(ei)
                    {
                        env.was_clip_active = clip_active;
                    }
                    continue;
                }
                let pd = &effect_def.param_defs[idx];
                let (min, max) = (pd.min, pd.max);
                let whole = pd.whole_numbers || pd.value_labels.is_some();

                let new_walk = if trigger {
                    if walk_value < 0.0 {
                        compute_random_step(
                            0.5, 1.0, true, whole, min, max, range_min, range_max,
                        )
                    } else {
                        compute_random_step(
                            walk_value, 0.15, random_jump, whole, min, max,
                            range_min, range_max,
                        )
                    }
                } else if walk_value < 0.0 {
                    if let Some(envs) = &mut layer.envelopes
                        && let Some(env) = envs.get_mut(ei)
                    {
                        env.was_clip_active = clip_active;
                        env.last_elapsed = elapsed_f;
                    }
                    continue;
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

                if let Some(envs) = &mut layer.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.walk_value = new_walk;
                    env.current_level = new_walk;
                    env.was_clip_active = clip_active;
                    env.last_elapsed = elapsed_f;
                }

                fx.param_values[idx].value = held;
                any_modulated = true;
                continue;
            }

            // ── ADSR mode ────────────────────────────────────────────
            if !clip_active {
                if let Some(envs) = &mut layer.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
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

            if let Some(envs) = &mut layer.envelopes
                && let Some(env) = envs.get_mut(ei)
            {
                env.current_level = adsr_value;
                env.was_clip_active = clip_active;
            }

            let layer_effects = match &mut layer.effects {
                Some(effects) => effects,
                None => continue,
            };
            let target_fx = layer_effects
                .iter_mut()
                .find(|f| f.effect_type() == &target_effect_type && f.enabled);
            let fx = match target_fx {
                Some(f) => f,
                None => continue,
            };
            let effect_def =
                match effect_definition_registry::try_get(fx.effect_type()) {
                    Some(d) => d,
                    None => continue,
                };
            let Some(&idx) = effect_def.id_to_index.get(param_id.as_ref()) else {
                continue;
            };
            if idx >= effect_def.param_defs.len() || idx >= fx.param_values.len() {
                continue;
            }
            let (min, max) = (
                effect_def.param_defs[idx].min,
                effect_def.param_defs[idx].max,
            );
            let current_value = fx.param_values[idx].value;
            let target_value = min + (max - min) * target_norm.clamp(0.0, 1.0);
            let offset = (target_value - current_value) * adsr_value;
            let final_value = (current_value + offset).clamp(min, max);

            if (final_value - current_value).abs() > f32::EPSILON {
                fx.param_values[idx].value = final_value;
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

        let clip_active = active_elapsed >= Beats::ZERO;

        let gp = match layer.gen_params_mut() {
            Some(gp) => gp,
            None => continue,
        };

        let env_count = gp.envelopes.as_ref().map_or(0, |e| e.len());
        if env_count == 0 {
            continue;
        }

        let gen_type = gp.generator_type();
        let gen_def = match generator_definition_registry::try_get(gen_type) {
            Some(d) => d,
            None => continue,
        };
        let gen_defs = &gen_def.param_defs;

        // Index-based iteration to avoid cloning (Phase 9C fix).
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
                let env = &gp.envelopes.as_ref().unwrap()[ei];
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
                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = clip_active;
                }
                continue;
            }

            let Some(&idx) = gen_def.id_to_index.get(param_id.as_ref()) else {
                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = clip_active;
                }
                continue;
            };
            if idx >= gen_defs.len() || idx >= gp.param_values.len() {
                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = clip_active;
                }
                continue;
            }

            let pd = &gen_defs[idx];
            let (min, max) = (pd.min, pd.max);

            // ── Random mode: sample & hold ─────────────────────────
            // Trigger conditions (same events that restart an ADSR envelope):
            //   1. Clip becomes active after being inactive (!was_active)
            //   2. Elapsed decreases — new sequential clip or loop restart
            //   3. First evaluation after mode switch (last_elapsed sentinel)
            if mode == EnvelopeMode::Random {
                let elapsed_f = active_elapsed.as_f32();
                let trigger = clip_active
                    && (last_elapsed < 0.0 || elapsed_f < last_elapsed || !was_active);
                let whole = pd.whole_numbers || pd.value_labels.is_some();

                let new_walk = if trigger {
                    if walk_value < 0.0 {
                        compute_random_step(
                            0.5, 1.0, true, whole, min, max, range_min, range_max,
                        )
                    } else {
                        compute_random_step(
                            walk_value, 0.15, random_jump, whole, min, max,
                            range_min, range_max,
                        )
                    }
                } else if walk_value < 0.0 {
                    if let Some(envs) = &mut gp.envelopes
                        && let Some(env) = envs.get_mut(ei)
                    {
                        env.was_clip_active = clip_active;
                        env.last_elapsed = elapsed_f;
                    }
                    continue;
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

                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.walk_value = new_walk;
                    env.current_level = new_walk;
                    env.was_clip_active = clip_active;
                    env.last_elapsed = elapsed_f;
                }

                gp.param_values[idx] = held;
                any_modulated = true;
                continue;
            }

            // ── ADSR mode ────────────────────────────────────────────
            if !clip_active {
                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = false;
                }
                continue;
            }

            let adsr_level = ParamEnvelope::calculate_adsr(
                active_elapsed,
                active_duration,
                attack,
                decay,
                sustain,
                release,
            );

            if let Some(envs) = &mut gp.envelopes
                && let Some(env) = envs.get_mut(ei)
            {
                env.current_level = adsr_level;
                env.was_clip_active = clip_active;
            }

            let current_value = gp.param_values[idx];
            let target_value = min + (max - min) * target_norm.clamp(0.0, 1.0);
            let offset = (target_value - current_value) * adsr_level;
            let final_value = (current_value + offset).clamp(min, max);

            if (final_value - current_value).abs() > f32::EPSILON {
                gp.param_values[idx] = final_value;
                any_modulated = true;
            }
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
