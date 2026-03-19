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

use manifold_core::effects::{EffectInstance, ParamEnvelope, ParameterDriver};
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{effect_definition_registry, generator_definition_registry};

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
        if layer.layer_type == LayerType::Generator {
            if let Some(gp) = &mut layer.gen_params {
                gp.reset_effectives();
            }
        }

        // Layer effect params: reset all (copy base → effective)
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                fx.reset_param_effectives();
            }
        }

        // Clip effect params: reset all
        for clip in layer.clips.iter_mut() {
            if clip.effects.is_empty() {
                continue;
            }
            for fx in clip.effects.iter_mut() {
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
pub fn evaluate_all_drivers(project: &mut Project, current_beat: f32) -> bool {
    let mut any_driven = false;

    // Master effect drivers
    for fx in project.settings.master_effects.iter_mut() {
        if evaluate_effect_drivers(fx, current_beat) {
            any_driven = true;
        }
    }

    // Layer effect drivers + generator param drivers
    for layer in project.timeline.layers.iter_mut() {
        if layer.is_muted {
            continue;
        }

        // Layer effect drivers
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if evaluate_effect_drivers(fx, current_beat) {
                    any_driven = true;
                }
            }
        }

        // Generator param drivers
        if layer.layer_type == LayerType::Generator {
            if let Some(gp) = &mut layer.gen_params {
                let gen_type = gp.generator_type;
                let gen_def = generator_definition_registry::get(gen_type);
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
    }

    any_driven
}

/// Evaluate all drivers on a single EffectInstance. Returns true if any driver was active.
/// Port of C# ParameterDriverManager.EvaluateEffectDrivers().
fn evaluate_effect_drivers(fx: &mut EffectInstance, current_beat: f32) -> bool {
    if !fx.enabled {
        return false;
    }
    let drivers = match &fx.drivers {
        Some(d) if !d.is_empty() => d,
        _ => return false,
    };

    let effect_def = effect_definition_registry::get(fx.effect_type);
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

/// Evaluate all clip and layer ADSR envelopes.
/// Returns true if any envelope was active (compositor should be marked dirty).
pub fn evaluate_all_envelopes(project: &mut Project, current_beat: f32) -> bool {
    let mut any_modulated = false;

    for layer in project.timeline.layers.iter_mut() {
        if layer.is_muted {
            continue;
        }

        // Find first active clip on this layer (for envelope timing)
        let mut active_elapsed: f32 = -1.0;
        let mut active_duration: f32 = 0.0;
        for clip in &layer.clips {
            if clip.is_muted {
                continue;
            }
            let elapsed = current_beat - clip.start_beat;
            if elapsed >= 0.0 && elapsed < clip.duration_beats {
                active_elapsed = elapsed;
                active_duration = clip.duration_beats;
                break; // Use first active clip
            }
        }

        // Evaluate clip envelopes (each clip may have its own envelopes on its effects).
        // Uses index-based iteration to avoid cloning the envelope list (Phase 9C fix).
        for clip in layer.clips.iter_mut() {
            if clip.is_muted || clip.effects.is_empty() {
                continue;
            }

            let env_count = clip.envelopes.as_ref().map_or(0, |e| e.len());
            if env_count == 0 {
                continue;
            }

            let clip_elapsed = current_beat - clip.start_beat;
            if clip_elapsed < 0.0 || clip_elapsed >= clip.duration_beats {
                continue;
            }

            for ei in 0..env_count {
                // Read envelope data by index (avoids borrow conflict with effects)
                let (enabled, target_effect_type, param_index, attack, decay, sustain, release, target_norm) = {
                    let env = &clip.envelopes.as_ref().unwrap()[ei];
                    (env.enabled, env.target_effect_type, env.param_index, env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats, env.target_normalized)
                };

                if !enabled {
                    continue;
                }

                let adsr_value = ParamEnvelope::calculate_adsr(
                    clip_elapsed,
                    clip.duration_beats,
                    attack,
                    decay,
                    sustain,
                    release,
                );

                // Write back currentLevel for UI visualization (Phase 9B fix).
                // Port of C# EnvelopeEvaluator line 96: env.currentLevel = adsrValue.
                if let Some(envs) = &mut clip.envelopes {
                    if let Some(env) = envs.get_mut(ei) {
                        env.current_level = adsr_value;
                    }
                }

                // Find the target effect on this clip
                let target_fx = clip.effects.iter_mut().find(|f| {
                    f.effect_type == target_effect_type && f.enabled
                });
                let fx = match target_fx {
                    Some(f) => f,
                    None => continue,
                };

                let effect_def = effect_definition_registry::get(fx.effect_type);
                let idx = param_index as usize;
                if idx >= effect_def.param_defs.len() || idx >= fx.param_values.len() {
                    continue;
                }

                let (min, max) = (effect_def.param_defs[idx].min, effect_def.param_defs[idx].max);

                // Additive composition: push current toward target
                let current_value = fx.param_values[idx];
                let target_value = min + (max - min) * target_norm.clamp(0.0, 1.0);
                let offset = (target_value - current_value) * adsr_value;
                let final_value = (current_value + offset).clamp(min, max);

                if (final_value - current_value).abs() > f32::EPSILON {
                    fx.param_values[idx] = final_value;
                    any_modulated = true;
                }
            }
        }

        // Evaluate layer envelopes (use timing from first active clip).
        // Uses index-based iteration to avoid cloning (Phase 9C fix).
        if active_elapsed >= 0.0 {
            let layer_env_count = layer.envelopes.as_ref().map_or(0, |e| e.len());
            if layer_env_count == 0 {
                continue;
            }

            if layer.effects.is_none() {
                continue;
            }

            for ei in 0..layer_env_count {
                let (enabled, target_effect_type, param_index, attack, decay, sustain, release, target_norm) = {
                    let env = &layer.envelopes.as_ref().unwrap()[ei];
                    (env.enabled, env.target_effect_type, env.param_index, env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats, env.target_normalized)
                };

                if !enabled {
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

                // Write back currentLevel (Phase 9B).
                // Port of C# EnvelopeEvaluator line 192.
                if let Some(envs) = &mut layer.envelopes {
                    if let Some(env) = envs.get_mut(ei) {
                        env.current_level = adsr_value;
                    }
                }

                let layer_effects = match &mut layer.effects {
                    Some(effects) => effects,
                    None => continue,
                };

                let target_fx = layer_effects.iter_mut().find(|f| {
                    f.effect_type == target_effect_type && f.enabled
                });
                let fx = match target_fx {
                    Some(f) => f,
                    None => continue,
                };

                let effect_def = effect_definition_registry::get(fx.effect_type);
                let idx = param_index as usize;
                if idx >= effect_def.param_defs.len() || idx >= fx.param_values.len() {
                    continue;
                }

                let (min, max) = (effect_def.param_defs[idx].min, effect_def.param_defs[idx].max);

                let current_value = fx.param_values[idx];
                let target_value = min + (max - min) * target_norm.clamp(0.0, 1.0);
                let offset = (target_value - current_value) * adsr_value;
                let final_value = (current_value + offset).clamp(min, max);

                if (final_value - current_value).abs() > f32::EPSILON {
                    fx.param_values[idx] = final_value;
                    any_modulated = true;
                }
            }
        }
    }

    any_modulated
}

// =====================================================================
// Phase 4: Evaluate generator param envelopes
// Port of C# EnvelopeEvaluator.EvaluateGenParamEnvelopes()
// =====================================================================

/// Evaluate ADSR envelopes on generator layer parameters.
/// Returns true if any envelope was active (compositor should be marked dirty).
pub fn evaluate_gen_param_envelopes(project: &mut Project, current_beat: f32) -> bool {
    let mut any_modulated = false;

    for layer in project.timeline.layers.iter_mut() {
        if layer.layer_type != LayerType::Generator || layer.is_muted {
            continue;
        }

        let gp = match &mut layer.gen_params {
            Some(gp) => gp,
            None => continue,
        };

        let env_count = gp.envelopes.as_ref().map_or(0, |e| e.len());
        if env_count == 0 {
            continue;
        }

        let gen_type = gp.generator_type;
        let gen_def = generator_definition_registry::get(gen_type);
        let gen_defs = &gen_def.param_defs;

        // Find first active clip on this layer for envelope timing
        let mut active_elapsed: f32 = -1.0;
        let mut active_duration: f32 = 0.0;
        for clip in &layer.clips {
            if clip.is_muted {
                continue;
            }
            let elapsed = current_beat - clip.start_beat;
            if elapsed >= 0.0 && elapsed < clip.duration_beats {
                active_elapsed = elapsed;
                active_duration = clip.duration_beats;
                break;
            }
        }

        // Index-based iteration to avoid cloning (Phase 9C fix).
        for ei in 0..env_count {
            let (enabled, param_index, attack, decay, sustain, release, target_norm) = {
                let env = &gp.envelopes.as_ref().unwrap()[ei];
                (env.enabled, env.param_index, env.attack_beats, env.decay_beats, env.sustain_level, env.release_beats, env.target_normalized)
            };

            if !enabled {
                continue;
            }

            let idx = param_index as usize;
            if idx >= gen_defs.len() || idx >= gp.param_values.len() {
                continue;
            }

            let (min, max) = (gen_defs[idx].min, gen_defs[idx].max);

            if active_elapsed < 0.0 {
                // No active clip — envelope at rest
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

            // Write back currentLevel (Phase 9B).
            // Port of C# EnvelopeEvaluator line 270.
            if let Some(envs) = &mut gp.envelopes {
                if let Some(env) = envs.get_mut(ei) {
                    env.current_level = adsr_level;
                }
            }

            // Additive composition: push current toward target
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
pub fn evaluate_modulation(project: &mut Project, current_beat: f32) -> bool {
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
