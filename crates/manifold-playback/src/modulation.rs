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
/// Returns the new walk_value in [0, 1].
fn compute_random_step(
    walk_value: f32,
    step_size: f32,
    random_jump: bool,
    whole_numbers: bool,
    min: f32,
    max: f32,
) -> f32 {
    if random_jump {
        let raw = random_unit();
        if whole_numbers && (max - min) > 0.0 {
            let steps = (max - min).round() as i32;
            if steps > 0 {
                let idx = (raw * (steps + 1) as f32).floor() as i32;
                let idx = idx.clamp(0, steps);
                return idx as f32 / steps as f32;
            }
        }
        return raw;
    }

    // Random walk: step up or down by step_size, clamp at boundaries.
    let up = random_unit() < 0.5;

    if whole_numbers && (max - min) > 0.0 {
        let steps = (max - min).round() as i32;
        if steps > 0 {
            let step_units = ((step_size * steps as f32).round() as i32).max(1);
            let current_idx = (walk_value * steps as f32).round() as i32;
            let new_idx = if up {
                (current_idx + step_units).min(steps)
            } else {
                (current_idx - step_units).max(0)
            };
            return new_idx as f32 / steps as f32;
        }
    }

    // Continuous walk — clamp at [0, 1]
    if up {
        (walk_value + step_size).min(1.0)
    } else {
        (walk_value - step_size).max(0.0)
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
                        let idx = driver.param_index as usize;
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
            let idx = driver.param_index as usize;
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
            fx.param_values[idx] = value;
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
pub fn evaluate_all_envelopes(project: &mut Project, current_beat: Beats) -> bool {
    let mut any_modulated = false;

    for layer in project.timeline.layers.iter_mut() {
        // Envelopes run even on muted layers (mute = compositor only).
        // Find first active clip on this layer (for envelope timing)
        let mut active_elapsed = Beats(-1.0); // sentinel: no active clip
        let mut active_duration = Beats::ZERO;
        for clip in &layer.clips {
            if clip.is_muted {
                continue;
            }
            let elapsed = current_beat - clip.start_beat;
            if elapsed >= Beats::ZERO && elapsed < clip.duration_beats {
                active_elapsed = elapsed;
                active_duration = clip.duration_beats;
                break; // Use first active clip
            }
        }

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
                param_index,
                attack,
                decay,
                sustain,
                release,
                target_norm,
                mode,
                random_jump,
                walk_value,
                was_active,
            ) = {
                let env = &layer.envelopes.as_ref().unwrap()[ei];
                (
                    env.enabled,
                    env.target_effect_type.clone(),
                    env.param_index,
                    env.attack_beats,
                    env.decay_beats,
                    env.sustain_level,
                    env.release_beats,
                    env.target_normalized,
                    env.mode,
                    env.random_jump,
                    env.walk_value,
                    env.was_clip_active,
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

            if !clip_active {
                if let Some(envs) = &mut layer.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = false;
                }
                continue;
            }

            // Random mode: on rising edge, pick a new target_norm via walk/jump.
            // Then the normal ADSR envelope drives toward that randomized target.
            let effective_target = if mode == EnvelopeMode::Random {
                let rising_edge = clip_active && !was_active;

                // Seed walk from current target if uninitialized
                let seeded = if walk_value < 0.0 {
                    target_norm.clamp(0.0, 1.0)
                } else {
                    walk_value
                };

                let new_walk = if rising_edge {
                    // Need param def for discrete quantization
                    let (whole, min, max) = layer
                        .effects
                        .as_ref()
                        .and_then(|efx| {
                            efx.iter()
                                .find(|f| f.effect_type() == &target_effect_type && f.enabled)
                        })
                        .and_then(|fx| {
                            effect_definition_registry::try_get(fx.effect_type())
                                .and_then(|d| {
                                    let idx = param_index as usize;
                                    d.param_defs.get(idx).map(|pd| {
                                        (
                                            pd.whole_numbers
                                                || pd.value_labels.is_some(),
                                            pd.min,
                                            pd.max,
                                        )
                                    })
                                })
                        })
                        .unwrap_or((false, 0.0, 1.0));

                    compute_random_step(
                        seeded,
                        target_norm.clamp(0.0, 1.0),
                        random_jump,
                        whole,
                        min,
                        max,
                    )
                } else {
                    seeded
                };

                if let Some(envs) = &mut layer.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.walk_value = new_walk;
                }

                new_walk
            } else {
                target_norm.clamp(0.0, 1.0)
            };

            // ADSR evaluation — shared by both ADSR and Random modes
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
            let idx = param_index as usize;
            if idx >= effect_def.param_defs.len() || idx >= fx.param_values.len() {
                continue;
            }
            let (min, max) = (
                effect_def.param_defs[idx].min,
                effect_def.param_defs[idx].max,
            );
            let current_value = fx.param_values[idx];
            let target_value = min + (max - min) * effective_target;
            let offset = (target_value - current_value) * adsr_value;
            let final_value = (current_value + offset).clamp(min, max);

            if (final_value - current_value).abs() > f32::EPSILON {
                fx.param_values[idx] = final_value;
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
pub fn evaluate_gen_param_envelopes(project: &mut Project, current_beat: Beats) -> bool {
    let mut any_modulated = false;

    for layer in project.timeline.layers.iter_mut() {
        if layer.layer_type != LayerType::Generator {
            continue;
        }

        // Find first active clip on this layer for envelope timing
        // (must borrow clips immutably before taking mutable gen_params borrow)
        let mut active_elapsed = Beats(-1.0); // sentinel: no active clip
        let mut active_duration = Beats::ZERO;
        for clip in &layer.clips {
            if clip.is_muted {
                continue;
            }
            let elapsed = current_beat - clip.start_beat;
            if elapsed >= Beats::ZERO && elapsed < clip.duration_beats {
                active_elapsed = elapsed;
                active_duration = clip.duration_beats;
                break;
            }
        }

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
                param_index,
                attack,
                decay,
                sustain,
                release,
                target_norm,
                mode,
                random_jump,
                walk_value,
                was_active,
            ) = {
                let env = &gp.envelopes.as_ref().unwrap()[ei];
                (
                    env.enabled,
                    env.param_index,
                    env.attack_beats,
                    env.decay_beats,
                    env.sustain_level,
                    env.release_beats,
                    env.target_normalized,
                    env.mode,
                    env.random_jump,
                    env.walk_value,
                    env.was_clip_active,
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

            let idx = param_index as usize;
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

            if !clip_active {
                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.was_clip_active = false;
                }
                continue;
            }

            // Random mode: on rising edge, pick a new target via walk/jump.
            // Then the normal ADSR envelope drives toward that randomized target.
            let effective_target = if mode == EnvelopeMode::Random {
                let rising_edge = clip_active && !was_active;
                let whole = pd.whole_numbers || pd.value_labels.is_some();

                let seeded = if walk_value < 0.0 {
                    target_norm.clamp(0.0, 1.0)
                } else {
                    walk_value
                };

                let new_walk = if rising_edge {
                    compute_random_step(
                        seeded,
                        target_norm.clamp(0.0, 1.0),
                        random_jump,
                        whole,
                        min,
                        max,
                    )
                } else {
                    seeded
                };

                if let Some(envs) = &mut gp.envelopes
                    && let Some(env) = envs.get_mut(ei)
                {
                    env.walk_value = new_walk;
                }

                new_walk
            } else {
                target_norm.clamp(0.0, 1.0)
            };

            // ADSR evaluation — shared by both modes
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
            let target_value = min + (max - min) * effective_target;
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
pub fn evaluate_modulation(project: &mut Project, current_beat: Beats) -> bool {
    // Phase 1: Reset all effective values to base
    reset_all_effectives(project);

    // Phase 2: Evaluate LFO drivers
    let any_driven = evaluate_all_drivers(project, current_beat);

    // Phase 3: Evaluate clip/layer ADSR envelopes (additive on top of drivers)
    let any_enveloped = evaluate_all_envelopes(project, current_beat);

    // Phase 4: Evaluate generator param ADSR envelopes
    let any_gen_enveloped = evaluate_gen_param_envelopes(project, current_beat);

    any_driven || any_enveloped || any_gen_enveloped
}
