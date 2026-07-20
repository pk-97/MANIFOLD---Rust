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
use manifold_core::audio_mod::{TriggerAction, WrapMode, random_step_value};
use manifold_core::audio_trigger::{FireMeterCapture, TriggerFireMode, fire_meter_key_for_param};
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
                let raw = driver_target_value(driver, current_beat, min, max);
                // BUG-039: a saw (or any waveform, via a trim overshoot)
                // bound to a periodic param wraps back into range instead
                // of clamping, so a full-range sweep spins continuously
                // instead of hitching at the rail.
                p.value = manifold_core::params::constrain_to_range(raw, min, max, p.spec.wraps);
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
    clip_edge_layers: &[i32],
    fire_meters: &mut FireMeterCapture,
) -> bool {
    // Phase 1: Reset all effective values to base
    reset_all_effectives(project);

    // Phase 1.5: Apply any armed step/random audio-mod shadow values (PARAM_
    // STEP_ACTIONS D4). Runs BEFORE drivers/continuous-mods/envelopes so a
    // step *replaces* the base exactly like a hand-moved slider — everything
    // downstream stacks on top unchanged. The shadow itself is only ever
    // advanced by this tick's `evaluate_all_audio_mods` call below, so a fire
    // surfaces here on the NEXT tick (one-frame latency, imperceptible at
    // 60fps, and it keeps the step arm from having to special-case "did I
    // just fire this same tick").
    let any_stepped = apply_step_values(project);

    // Phase 2: Evaluate LFO drivers
    let any_driven = evaluate_all_drivers(project, current_beat);

    // Phase 2.5: Evaluate audio modulations (live audio → effective). Driver-
    // like (sets the value), so it runs alongside drivers and before the
    // additive envelope phase. Inert when no audio features are present.
    // §9 U1/U6: a trigger-gate target's fire is collected into
    // `trigger_pulses` by this SAME walk (the deleted `AudioTriggerMod`
    // config's separate `evaluate_all_param_triggers` pass is gone — a
    // fire-mode mod is a normal audio mod now). A trigger-gate fire never
    // marks the compositor dirty on its own (the renderer's own dirty
    // tracking covers the visible effect). `clip_edge_layers` (PARAM_STEP_
    // ACTIONS D5) is the engine-computed set of `timeline.layers` indices
    // with a clip-start edge since this function's last call — the Step/
    // Random arm's second fire source, gated by `trigger_mode` (D3).
    let any_audio =
        evaluate_all_audio_mods(project, audio, dt, trigger_pulses, clip_edge_layers, fire_meters);

    // Pre-compute per-layer active clip timing for envelope phases.
    // Avoids O(total_clips) scan in each envelope function.
    compute_active_clip_timing(&project.timeline.layers, current_beat, timing_scratch);

    // Phase 3: Evaluate clip/layer/generator ADSR envelopes (additive on top
    // of drivers). One walk visits every layer's effects AND its generator
    // instance — see evaluate_all_envelopes.
    let any_enveloped = evaluate_all_envelopes(project, timing_scratch);

    any_stepped || any_driven || any_audio || any_enveloped
}

// =====================================================================
// Phase 1.5: Apply armed step/random audio-mod shadow values
// =====================================================================

/// For every enabled audio mod carrying an armed step/random shadow
/// (`step_value = Some(v)`), write `p.value = v` — the step *replaces* the
/// base for this tick (D4). Walks the same instance set
/// [`evaluate_all_audio_mods`]/[`evaluate_all_drivers`] do (master effects +
/// layer effects + generator params); a disabled instance or a mod with no
/// shadow yet (`None` — never fired, or fell back after a disarm/reload)
/// leaves the param at the base `reset_all_effectives` just wrote. Returns
/// true if any value was written (compositor should be marked dirty).
pub fn apply_step_values(project: &mut Project) -> bool {
    fn apply_instance(fx: &mut PresetInstance) -> bool {
        if !fx.enabled {
            return false;
        }
        let Some(mods) = fx.audio_mods.as_ref() else {
            return false;
        };
        let params = &mut fx.params;
        let mut any = false;
        for m in mods.iter().filter(|m| m.enabled) {
            if let Some(v) = m.step_value
                && let Some(p) = params.get_mut(m.param_id.as_ref())
            {
                p.value = v;
                any = true;
            }
        }
        any
    }

    let mut any = false;
    for fx in project.settings.master_effects.iter_mut() {
        if apply_instance(fx) {
            any = true;
        }
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if apply_instance(fx) {
                    any = true;
                }
            }
        }
        if let Some(gp) = layer.gen_params_mut()
            && apply_instance(gp)
        {
            any = true;
        }
    }
    any
}

// =====================================================================
// Phase 2.5: Evaluate audio modulations (live audio features → effective)
// =====================================================================

/// Evaluate every audio modulation on master effects, layer effects, and
/// generator params, using the latest per-send feature `snapshot`. Returns true
/// if any modulation wrote a value (compositor should be marked dirty).
/// Clears and refills `pulses` with every trigger-gate fire this tick (§9 U1)
/// — a fire never sets the return bool (see [`evaluate_instance_audio_mods`]).
///
/// Walks the same instance set as the driver pass (master + layer effects +
/// generators) — NOT clip effects, which the modulation pipeline does not reset
/// or evaluate. A modulation whose send no longer resolves (deleted send, or no
/// features yet) is skipped, leaving its param at the base value from
/// `reset_all_effectives` — the orphan policy, matching drivers/envelopes.
///
/// `clip_edge_layers` (PARAM_STEP_ACTIONS D5) is the engine's list of
/// `timeline.layers` indices with a clip-start edge since this was last
/// called — a Clip/Both-mode Step or Random mod on that layer fires this
/// tick (D3). Master-chain instances have no layer index, so they never
/// see a clip contribution (D3/D5's "clip contribution is 0" rule) —
/// **note:** this early-return (inherited from P1, unchanged here) still
/// gates a pure Clip-mode step on `by_send` resolving the mod's configured
/// source, because every `ParameterAudioMod` is fundamentally send-sourced
/// (P1 scope); a step mod whose bound send has no feature data yet won't
/// fire on a clip edge either. Not a P2 regression — flagging so it isn't
/// mistaken for one.
pub fn evaluate_all_audio_mods(
    project: &mut Project,
    snapshot: &AudioFeatureSnapshot,
    dt: Seconds,
    pulses: &mut Vec<TriggerPulse>,
    clip_edge_layers: &[i32],
    fire_meters: &mut FireMeterCapture,
) -> bool {
    pulses.clear();
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
        if evaluate_instance_audio_mods(fx, &by_send, dt, None, false, pulses, fire_meters) {
            any = true;
        }
    }
    for (layer_index, layer) in project.timeline.layers.iter_mut().enumerate() {
        let layer_id = layer.layer_id.clone();
        let clip_edge = clip_edge_layers.contains(&(layer_index as i32));
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if evaluate_instance_audio_mods(
                    fx,
                    &by_send,
                    dt,
                    Some(layer_id.clone()),
                    clip_edge,
                    pulses,
                    fire_meters,
                ) {
                    any = true;
                }
            }
        }
        if let Some(gp) = layer.gen_params_mut()
            && evaluate_instance_audio_mods(
                gp,
                &by_send,
                dt,
                Some(layer_id),
                clip_edge,
                pulses,
                fire_meters,
            )
        {
            any = true;
        }
    }

    any
}

/// Evaluate every audio modulation on a single instance. Returns true if any
/// wrote a param value (a trigger-gate fire pushed onto `pulses` does NOT
/// count — see §9 U1). `layer_id` is `None` for a master-chain instance,
/// cloned onto every pulse this instance emits. `clip_edge` (PARAM_STEP_
/// ACTIONS D5) is whether this instance's owning layer had a clip-start edge
/// since this was last called — always `false` for a master-chain instance
/// (`layer_id: None`), matching D3's "master chains have no layer" rule.
fn evaluate_instance_audio_mods(
    fx: &mut PresetInstance,
    by_send: &HashMap<AudioSendId, SendFeatures>,
    dt: Seconds,
    layer_id: Option<manifold_core::id::LayerId>,
    clip_edge: bool,
    pulses: &mut Vec<TriggerPulse>,
    fire_meters: &mut FireMeterCapture,
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
        let (min, max, is_trigger, is_trigger_gate, whole_numbers) = match params.get(m.param_id.as_ref()) {
            Some(p) => (p.spec.min, p.spec.max, p.spec.is_trigger, p.spec.is_trigger_gate, p.whole_numbers()),
            None => continue,
        };
        let raw = m.source.feature.extract(features);
        let shape = m.shape;
        // BUG-242: `m.prev_raw` before `condition()` mutates it — the
        // trigger-gate arm below needs the pre-tick value to recompute the
        // sensitivity-scaled raw level for edge detection.
        let prev_raw_before_condition = m.prev_raw;
        // `conditioned` is the pre-range-map signal (sensitivity, smoothing,
        // invert, curve) — edge detection (trigger/Step/Random fire arms
        // below) reads THIS, never the range-mapped `out_norm`, so the trim
        // handles never distort whether/when a mod fires (BUG: range_min >=
        // 0.5 fired once and never re-armed; range_max <= 0.5 never fired at
        // all). `out_norm` stays the range-mapped value for Continuous, which
        // is exactly what the range map is for. The `is_trigger_gate` arm
        // below is the one exception (BUG-242): it edge-detects the
        // sensitivity-scaled RAW level instead, decoupled from this
        // envelope, so a shape's release can't swallow a second onset.
        let conditioned = shape.condition(raw, dt_s, &mut m.smoothed, &mut m.prev_raw);
        let out_norm = shape.map_range(conditioned);

        // D6 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` P3c,
        // BUG-082's fix; widened 2026-07-11 — this used to sit inside the
        // `is_trigger_gate` arm below, so a continuous/Step/Random drawer's
        // Amount meter never had a level to show): capture the SAME
        // pre-range-map signal any edge detector below reads, keyed on this
        // mod's owning effect/generator + param — the drawer meter shows
        // exactly what the mod is reading, gate or not. The push happens per
        // arm below (2026-07-19, param-drawer unification): the
        // `is_trigger_gate` arm fires on the sensitivity-scaled RAW edge
        // (BUG-242), so its meter shows THAT, not the shaped envelope the
        // gate ignores — tuning sensitivity against a smoothed meter lied
        // about where the fire threshold sat. Every other arm keeps
        // `conditioned`, the signal they actually read.
        if is_trigger_gate {
            // §9 U1: the mod's target is a trigger-gate card (e.g.
            // `clip_trigger`) — never write the toggle's value (R2's
            // continuous-mod flapping stays dead; the toggle is a user
            // control). Same D5b fire chassis as `is_trigger` below, but a
            // fire pushes a pulse for the renderer's `audio_count` instead of
            // a monotonic count. Mode gates only whether the pulse EMITS, not
            // whether the edge advances, so switching mode live never leaves
            // the armed flag out of sync with the audio signal.
            //
            // BUG-242: detection runs on the sensitivity-scaled RAW level,
            // not `conditioned` — the shape's attack/release envelope
            // (release defaults to 120 ms) otherwise gates how fast the edge
            // can re-arm, deafening dense material. Same sensitivity/
            // rate-of-change step `AudioModShape::condition`'s `target`
            // computes internally (manifold-core's `audio_mod.rs`); trim
            // handles (range_min/range_max) still never distort firing since
            // this is pre-range-map either way.
            let edge_level = if shape.rate_of_change {
                let rate = (raw - prev_raw_before_condition) / dt_s.max(1e-4);
                (0.5 + rate * shape.sensitivity).clamp(0.0, 1.0)
            } else {
                (raw * shape.sensitivity).clamp(0.0, 1.0)
            };
            fire_meters.push(
                fire_meter_key_for_param(fx.id.as_str(), m.param_id.as_ref()),
                edge_level,
            );
            if m.trigger_edge.advance(edge_level, 0.5)
                && m.trigger_mode.unwrap_or(TriggerFireMode::Both).wants_transient()
            {
                pulses.push(TriggerPulse { layer_id: layer_id.clone() });
            }
            continue;
        }

        fire_meters.push(fire_meter_key_for_param(fx.id.as_str(), m.param_id.as_ref()), conditioned);

        if is_trigger {
            // §8 D5b: a fire-button target wants a monotonic count, not a
            // level — edge-detect the shaped signal (rising through
            // mid-range) instead of overwriting continuously. Downstream
            // `last_count`-style consumers edge-detect the written value
            // unchanged. `action` is irrelevant here — the drawer never
            // offers an Action row on an `is_trigger`/`is_trigger_gate` card
            // (D8), so this arm's behavior is untouched by PARAM_STEP_ACTIONS.
            // Detection runs on `conditioned` (pre-range-map) so trim handles
            // never distort firing.
            if let Some(p) = params.get_mut(m.param_id.as_ref()) {
                if m.trigger_edge.advance(conditioned, 0.5) {
                    m.fire_count = m.fire_count.wrapping_add(1);
                }
                p.value = p.base + m.fire_count as f32;
                wrote = true;
            }
            continue;
        }

        match m.action {
            TriggerAction::Continuous => {
                if let Some(p) = params.get_mut(m.param_id.as_ref()) {
                    p.value = min + (max - min) * out_norm;
                    wrote = true;
                }
            }
            TriggerAction::Step { amount, wrap } => {
                // PARAM_STEP_ACTIONS D4/D2: a fire advances the runtime shadow
                // only — it does NOT write `p.value` this tick (that's Phase
                // 1.5's job, next tick). First fire seeds from the committed
                // `base` (D4's lifecycle), never from `p.value` (which may
                // already be mid-frame-modulated by an earlier phase).
                //
                // D3: `trigger_edge.advance` always runs (keeps the armed/
                // re-arm state tracking the live signal regardless of mode,
                // exactly like the is_trigger_gate arm above) — `trigger_mode`
                // only gates which of the two admitted edges actually fires
                // the step. `None` defaults to Transient here, NOT `Both`
                // like gate cards (D3: a step mod with no audio intent is
                // meaningless — arming it required opening an audio drawer).
                // Detection runs on `conditioned` (pre-range-map) so trim
                // handles never distort firing.
                let audio_edge = m.trigger_edge.advance(conditioned, 0.5);
                let mode = m.trigger_mode.unwrap_or(TriggerFireMode::Transient);
                let fires =
                    (mode.wants_transient() && audio_edge) || (mode.wants_clip_edge() && clip_edge);
                if fires {
                    // The trim-handle zone is what Step travels within, not
                    // the param's full min/max rails — a trimmed handle bounds
                    // Step/Random exactly the way it already bounds Continuous.
                    let (mut lo, mut hi) = shape.zone(min, max);
                    if whole_numbers {
                        // Snap the zone rails to the integer grid inside it.
                        let lo_i = lo.ceil();
                        let hi_i = hi.floor();
                        if hi_i < lo_i {
                            // No integer lands inside this (sub-1-wide) zone —
                            // collapse to the nearest integer to its center.
                            let collapsed = ((lo + hi) * 0.5).round().clamp(min, max);
                            lo = collapsed;
                            hi = collapsed;
                        } else {
                            lo = lo_i;
                            hi = hi_i;
                        }
                    }
                    let base = params.get(m.param_id.as_ref()).map(|p| p.base).unwrap_or(lo);
                    // A base outside the zone (e.g. left over from before the
                    // handles were trimmed) enters at the nearest rail.
                    let current = m.step_value.unwrap_or(base).clamp(lo, hi);
                    // A discrete zone's reachable set is the INCLUSIVE integer
                    // grid {lo, lo+1, ..., hi} — hi-lo+1 positions, one more
                    // than a continuous cycle's hi-lo. `Wrap` must land
                    // one-past-hi exactly on lo, so its cycle length needs
                    // that +1; `Bounce`/`Clamp` reflect/saturate at the true
                    // hi regardless (hi IS a reachable, real rail either way).
                    let wrap_max = if whole_numbers && matches!(wrap, WrapMode::Wrap) {
                        hi + 1.0
                    } else {
                        hi
                    };
                    let (mut next, dir) = wrap.advance(current, amount, m.step_dir, lo, wrap_max);
                    if whole_numbers {
                        next = next.round();
                    }
                    m.step_dir = dir;
                    m.step_value = Some(next.clamp(lo, hi));
                }
            }
            TriggerAction::Random => {
                // D7: fire_count doubles as the deterministic hash ordinal
                // here — this arm and the `is_trigger` monotonic-count arm
                // above are mutually exclusive per mod (is_trigger `continue`s
                // before this match), so there's no shared-meaning collision.
                // D3 event-source gating: same shape as the Step arm above.
                // Detection runs on `conditioned` (pre-range-map) so trim
                // handles never distort firing.
                let audio_edge = m.trigger_edge.advance(conditioned, 0.5);
                let mode = m.trigger_mode.unwrap_or(TriggerFireMode::Transient);
                let fires =
                    (mode.wants_transient() && audio_edge) || (mode.wants_clip_edge() && clip_edge);
                if fires {
                    m.fire_count = m.fire_count.wrapping_add(1);
                    // Same trim-handle zone as Step: Random jumps within the
                    // rails the handles define, not the param's full range.
                    let (mut lo, mut hi) = shape.zone(min, max);
                    if whole_numbers {
                        let lo_i = lo.ceil();
                        let hi_i = hi.floor();
                        if hi_i < lo_i {
                            let collapsed = ((lo + hi) * 0.5).round().clamp(min, max);
                            lo = collapsed;
                            hi = collapsed;
                        } else {
                            lo = lo_i;
                            hi = hi_i;
                        }
                    }
                    let base = params.get(m.param_id.as_ref()).map(|p| p.base).unwrap_or(lo);
                    let current = m.step_value.unwrap_or(base).clamp(lo, hi);
                    let discrete_count =
                        whole_numbers.then(|| ((hi - lo).round().max(0.0) as u32) + 1);
                    let current_index = discrete_count
                        .map(|n| ((current - lo).round().max(0.0) as u32).min(n.saturating_sub(1)))
                        .unwrap_or(0);
                    let next = random_step_value(m.fire_count, lo, hi, discrete_count, current_index);
                    m.step_value = Some(next.clamp(lo, hi));
                }
            }
        }
    }
    wrote
}

/// One instance's trigger-gate fire this tick (§9 U1, formerly §8 D1's
/// `audio_trigger` pulse), for the renderer (P2) to fold into its
/// `audio_count`. `layer_id` is `None` for a master-chain instance (D5:
/// "master/global chains have no layer... audio fires still work") — the
/// renderer keys a master-scoped counter off that sentinel. Collected by
/// [`evaluate_all_audio_mods`] itself now — no separate walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerPulse {
    pub layer_id: Option<manifold_core::id::LayerId>,
}

/// BUG-051 fix (§8 D4, still true post-§9): drop every audio-mod's trigger
/// edge-detector back to armed. Call on transport stop / project reset so a
/// stale "fired, not yet re-armed" flag can't suppress the first onset next
/// time. Walks the same instance set [`evaluate_all_audio_mods`] does,
/// clearing every mod's `trigger_edge` — the single edge-state holder that
/// now backs both `is_trigger` fire buttons (D5b) and `is_trigger_gate`
/// cards (§9 U1); the former separate `audio_trigger.edge` holder was
/// deleted along with the config type it lived on.
///
/// PARAM_STEP_ACTIONS D4: also drops each mod's step shadow
/// (`step_value`/`step_dir`) back to its initial `None`/`+1` state, at the
/// same call site — a transport stop is exactly the "kill the trigger"
/// moment D4 documents: the param falls back to its committed base rather
/// than resuming mid-sequence next time the transport starts.
pub fn clear_all_trigger_edges(project: &mut Project) {
    fn clear_instance(fx: &mut PresetInstance) {
        if let Some(mods) = fx.audio_mods.as_mut() {
            for m in mods.iter_mut() {
                m.trigger_edge.clear();
                m.step_value = None;
                m.step_dir = 1.0;
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
    use manifold_core::{BeatDivision, DriverWaveform};
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

    /// An envelope binds to the
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

        let mut fire_meters = FireMeterCapture::default();
        let snap = snapshot_low(1.0);
        let active = evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016), &mut Vec::new(), &[], &mut fire_meters);
        assert!(active, "an audio mod with signal reports modulation");

        // Widened 2026-07-11 (was: only `is_trigger_gate` mods captured a
        // level, so continuous/Step/Random drawers had no meter): this mod is
        // a plain Continuous `TriggerAction` on a non-gate param, and it must
        // still leave its conditioned level in `fire_meters` — the meter
        // strip on every audio-mod drawer, not just fire-mode ones, reads
        // from here.
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        let key = manifold_core::audio_trigger::fire_meter_key_for_param(fx.id.as_str(), "amount");
        assert!(
            fire_meters.get(key).is_some_and(|l| (l - 1.0).abs() < 1e-6),
            "a plain continuous mod must capture its conditioned level too, got {:?}",
            fire_meters.get(key)
        );

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
        assert!(!evaluate_all_audio_mods(&mut project, &empty, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default()));
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
        assert!(!evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default()));
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
        assert!(!evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default()));
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
        assert!(evaluate_all_audio_mods(&mut project, &snap, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default()));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.params.get("amount").unwrap().value - 0.5).abs() < 1e-6,
            "full signal caps at the recalibrated max 0.5, got {}",
            fx.params.get("amount").unwrap().value,
        );
    }

    /// BUG-104 INVESTIGATION (Lane C, scratch — not a fix). Pins the
    /// ENGINE half of the "fader-riding card mod goes dead and stays dead
    /// when a trigger is enabled" report. The Continuous arm
    /// (`modulation.rs`) has no coupling to any trigger state, and the
    /// per-instance `audio_mods` list is not rebuilt when a trigger toggles,
    /// so a card audio mod on a plain Float param keeps tracking its signal
    /// every tick REGARDLESS of trigger enable/disable — and
    /// `clear_all_trigger_edges` (the transport-stop / "kill the trigger"
    /// reset) clears only `trigger_edge`/step shadows, never `m.enabled` and
    /// never `p.value`. Conclusion: any BUG-104 takeover/persistence must
    /// live on the RENDERER graph side (mux `switch_value` selecting the
    /// trigger cycle over the user binding, plus the `clip_trigger_cycle`/
    /// `sample_and_hold` StateStore latch that playback's reset cannot
    /// reach), not in the engine mod evaluator.
    #[test]
    fn bug104_continuous_card_mod_is_decoupled_from_trigger_reset() {
        let (mut project, send_id) = project_with_audio_send();
        attach_full_range_low_mod(&mut project, &send_id);

        // The fader tracks its signal every tick — up, down, up.
        for (level, expect) in [(1.0_f32, 1.0_f32), (0.25, 0.25), (0.8, 0.8)] {
            let snap = snapshot_low(level);
            evaluate_all_audio_mods(
                &mut project,
                &snap,
                Seconds(0.016),
                &mut Vec::new(),
                &[],
                &mut FireMeterCapture::default(),
            );
            let v = project.timeline.layers[0].effects.as_ref().unwrap()[0]
                .params
                .get("amount")
                .unwrap()
                .value;
            assert!(
                (v - expect).abs() < 1e-6,
                "continuous card mod tracks signal each tick: expected {expect}, got {v}"
            );
        }

        // The transport-stop / trigger-kill reset runs.
        clear_all_trigger_edges(&mut project);

        // The mod is untouched: still enabled, still tracking.
        assert!(
            project.timeline.layers[0].effects.as_ref().unwrap()[0].audio_mods.as_ref().unwrap()[0]
                .enabled,
            "trigger-edge reset must not disable the continuous card mod"
        );
        let snap = snapshot_low(0.5);
        let active = evaluate_all_audio_mods(
            &mut project,
            &snap,
            Seconds(0.016),
            &mut Vec::new(),
            &[],
            &mut FireMeterCapture::default(),
        );
        assert!(active, "continuous card mod still live after the trigger reset");
        let v = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap()
            .value;
        assert!(
            (v - 0.5).abs() < 1e-6,
            "continuous card mod still tracks its signal after the reset, got {v}"
        );
    }

    // ── §9 U1: unified trigger-gate mods (formerly §8's separate
    //    `AudioTriggerMod` config, deleted) ──────────────────────────────

    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::params::Param;

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

    /// A snapshot with one send whose Full-band transient reads `level`.
    fn snapshot_full_transient(level: f32) -> AudioFeatureSnapshot {
        let mut s = AudioFeatureSnapshot { sends: vec![SendFeatures::default()] };
        s.sends[0].bands[AudioBand::Full.index()].transients = level;
        s
    }

    /// A bundled `is_trigger_gate` param — the only way to get one outside
    /// the JSON preset path (the compile-time `ParamSpec` inventory format
    /// has no field for it; see
    /// `generator_registration::ParamSpec::to_param_def`'s doc comment).
    fn add_trigger_gate_param(inst: &mut PresetInstance, id: &str) {
        inst.params.push(Param::bundled(ParamSpecDef {
            id: id.to_string(),
            name: "Clip Trigger".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            whole_numbers: false,
            is_toggle: true,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: true,
            wraps: false,
            section: None,
        }));
    }

    /// A fire-mode mod targeting `clip_trigger`, instant (no smoothing) so a
    /// hot transient reads straight through to the 0.5 edge threshold.
    fn gate_mod(send_id: &AudioSendId, mode: TriggerFireMode) -> ParameterAudioMod {
        let mut m = ParameterAudioMod::new(
            "clip_trigger".into(),
            send_id.clone(),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.trigger_mode = Some(mode);
        m.shape = AudioModShape { attack_ms: 0.0, release_ms: 0.0, ..Default::default() };
        m
    }

    #[test]
    fn trigger_gate_mod_fires_on_generator_and_produces_layer_pulse() {
        let mut layer = generator_layer();
        add_trigger_gate_param(layer.gen_params_or_init(), "clip_trigger");
        let mut project = project_with(layer);
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        let layer_id = project.timeline.layers[0].layer_id.clone();
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(gate_mod(&send_id, TriggerFireMode::Both));

        let hot = snapshot_full_transient(0.99);
        let mut pulses = Vec::new();
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert_eq!(pulses, vec![TriggerPulse { layer_id: Some(layer_id) }]);
    }

    #[test]
    fn trigger_gate_mod_fires_once_then_rearms_below_ratio() {
        let mut layer = generator_layer();
        add_trigger_gate_param(layer.gen_params_or_init(), "clip_trigger");
        let mut project = project_with(layer);
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(gate_mod(&send_id, TriggerFireMode::Both));

        let hot = snapshot_full_transient(0.99);
        let mut pulses = Vec::new();
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert_eq!(pulses.len(), 1, "rising edge fires");

        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert!(pulses.is_empty(), "still hot (held high), no re-fire");
    }

    #[test]
    fn trigger_gate_mod_mode_clip_edge_never_pulses() {
        let mut layer = generator_layer();
        add_trigger_gate_param(layer.gen_params_or_init(), "clip_trigger");
        let mut project = project_with(layer);
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(gate_mod(&send_id, TriggerFireMode::ClipEdge));

        let hot = snapshot_full_transient(0.99);
        let mut pulses = Vec::new();
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert!(pulses.is_empty(), "ClipEdge mode (default) must not react to audio");
    }

    #[test]
    fn trigger_gate_mod_master_effect_pulse_has_no_layer() {
        let mut project = Project::default();
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        let mut fx = PresetInstance::new(PresetTypeId::new("TestMasterGate"));
        add_trigger_gate_param(&mut fx, "clip_trigger");
        fx.audio_mods_mut().push(gate_mod(&send_id, TriggerFireMode::Transient));
        project.settings.master_effects.push(fx);

        let hot = snapshot_full_transient(0.99);
        let mut pulses = Vec::new();
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert_eq!(pulses, vec![TriggerPulse { layer_id: None }]);
    }

    #[test]
    fn clear_all_trigger_edges_rearms_gate_mod() {
        let mut layer = generator_layer();
        add_trigger_gate_param(layer.gen_params_or_init(), "clip_trigger");
        let mut project = project_with(layer);
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(gate_mod(&send_id, TriggerFireMode::Transient));

        let hot = snapshot_full_transient(0.99);
        let mut pulses = Vec::new();
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert_eq!(pulses.len(), 1);
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert!(pulses.is_empty(), "disarmed");

        clear_all_trigger_edges(&mut project);
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut pulses, &[], &mut FireMeterCapture::default());
        assert_eq!(
            pulses.len(),
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

        assert!(evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default()));
        let value = |p: &Project| p.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("fire")
            .unwrap()
            .value;
        assert_eq!(value(&project), 1.0, "first rising edge bumps the count to 1");

        // Still hot: no re-fire, count holds at 1.
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        assert_eq!(value(&project), 1.0);

        // Decay below the rearm floor, then fire again: count bumps to 2.
        evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        assert_eq!(value(&project), 2.0);
    }

    // ── PARAM_STEP_ACTIONS P1: step/random actions on `ParameterAudioMod` ──

    /// Attach a step/random mod on `param_id` (must exist on `TestEnvFx`),
    /// reading the send's Full-band transient with an instant (unsmoothed)
    /// shape so a snapshot level crossing the 0.5 edge threshold fires
    /// deterministically on the very next evaluator call — the same D5b
    /// chassis `is_trigger`/`is_trigger_gate` already use.
    fn attach_action_mod(
        project: &mut Project,
        send_id: &AudioSendId,
        param_id: &'static str,
        action: TriggerAction,
    ) {
        let mut m = ParameterAudioMod::new(
            param_id.into(),
            send_id.clone(),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.shape = AudioModShape { attack_ms: 0.0, release_ms: 0.0, ..Default::default() };
        m.action = action;
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods_mut()
            .push(m);
    }

    /// The layer-0 effect's first (and only, in these tests) audio mod's
    /// current step shadow.
    fn step_value_of(project: &Project) -> Option<f32> {
        project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .audio_mods
            .as_ref()
            .unwrap()[0]
            .step_value
    }

    fn step_dir_of(project: &Project) -> f32 {
        project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .audio_mods
            .as_ref()
            .unwrap()[0]
            .step_dir
    }

    /// Drive `n` fires on the layer-0 effect's first audio mod, alternating
    /// hot/cold snapshots so `TransientEdge` re-arms between each (it only
    /// re-arms once the level drops below `threshold * REARM_RATIO`). Returns
    /// the mod's `step_value` after each hot call, in fire order.
    fn fire_n_times(project: &mut Project, n: usize) -> Vec<f32> {
        let hot = snapshot_full_transient(0.9);
        let cold = snapshot_full_transient(0.0);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            evaluate_all_audio_mods(project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
            out.push(step_value_of(project).expect("armed after a fire"));
            evaluate_all_audio_mods(project, &cold, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        }
        out
    }

    #[test]
    fn step_shadow_advances_only_on_fire_not_every_tick() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs", // whole_numbers 0..8
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );

        let hot = snapshot_full_transient(0.9);
        // First rising edge: shadow seeds from base (0) and steps by 1.
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        assert_eq!(step_value_of(&project), Some(1.0));

        // Signal stays hot: the edge is disarmed, so the SAME call again must
        // not advance the shadow a second time.
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        assert_eq!(
            step_value_of(&project),
            Some(1.0),
            "a sustained signal must not re-step every tick"
        );
    }

    #[test]
    fn step_wrap_mode_cycles_past_max_to_min_for_discrete_param() {
        let (mut project, send_id) = project_with_audio_send();
        // segs: 0..8 whole-numbers → 9 reachable positions. Wrap must treat
        // one step past 8 as landing exactly on 0 (the *discrete* cycle
        // length is max-min+1, not max-min — see the Step arm's `wrap_max`
        // adjustment).
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Wrap },
        );

        let expect = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 0.0];
        let got = fire_n_times(&mut project, expect.len());
        assert_eq!(got, expect, "9 fires of +1 wrap exactly once around the 9-position cycle");
    }

    #[test]
    fn step_bounce_mode_reverses_direction_at_rails() {
        let (mut project, send_id) = project_with_audio_send();
        // "amount": continuous 0..1. amount=0.7 overshoots both rails within
        // 3 fires, so the direction flip is observable directly.
        attach_action_mod(
            &mut project,
            &send_id,
            "amount",
            TriggerAction::Step { amount: 0.7, wrap: WrapMode::Bounce },
        );

        let got = fire_n_times(&mut project, 3);
        // fire1: 0 + 0.7*1 = 0.7 (no rail hit yet, dir stays +1)
        assert!((got[0] - 0.7).abs() < 1e-5, "fire1 = {}", got[0]);
        // fire2: 0.7 + 0.7 = 1.4 overshoots max(1) by 0.4 -> reflects to 0.6, dir flips to -1
        assert!((got[1] - 0.6).abs() < 1e-5, "fire2 = {}", got[1]);
        // fire3: 0.6 + 0.7*(-1) = -0.1 undershoots min(0) by 0.1 -> reflects to 0.1, dir flips to +1
        assert!((got[2] - 0.1).abs() < 1e-5, "fire3 = {}", got[2]);
        assert!((step_dir_of(&project) - 1.0).abs() < 1e-5, "dir flipped back to +1 after the 2nd rail");
    }

    #[test]
    fn step_clamp_mode_saturates_at_rails() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "amount", // continuous 0..1
            TriggerAction::Step { amount: 0.9, wrap: WrapMode::Clamp },
        );

        let got = fire_n_times(&mut project, 3);
        assert!((got[0] - 0.9).abs() < 1e-5, "fire1 within range, got {}", got[0]);
        assert_eq!(got[1], 1.0, "fire2 saturates at max instead of wrapping/bouncing");
        assert_eq!(got[2], 1.0, "fire3 stays saturated, no bounce-back");
    }

    #[test]
    fn step_amount_scales_the_per_fire_jump_proportionally() {
        // The control surface for "how big a jump" is `amount` alone — there
        // is no separate "how often" knob (D6 retired the `every` divisor).
        // A small vs. large amount from the same base must produce a
        // proportionally different single-fire jump.
        let (mut small_project, send_id_a) = project_with_audio_send();
        attach_action_mod(
            &mut small_project,
            &send_id_a,
            "amount",
            TriggerAction::Step { amount: 0.1, wrap: WrapMode::Clamp },
        );
        let (mut large_project, send_id_b) = project_with_audio_send();
        attach_action_mod(
            &mut large_project,
            &send_id_b,
            "amount",
            TriggerAction::Step { amount: 0.4, wrap: WrapMode::Clamp },
        );

        let small = fire_n_times(&mut small_project, 1)[0];
        let large = fire_n_times(&mut large_project, 1)[0];
        assert!((small - 0.1).abs() < 1e-5, "small amount jump, got {small}");
        assert!((large - 0.4).abs() < 1e-5, "large amount jump, got {large}");
        assert!(large > small * 3.0, "a 4x amount must produce a markedly larger jump");
    }

    #[test]
    fn apply_step_values_writes_armed_shadow_then_disarm_falls_back_to_base() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );

        let hot = snapshot_full_transient(0.9);
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        assert_eq!(step_value_of(&project), Some(1.0));

        // Phase 1 then Phase 1.5, exactly as `evaluate_modulation` orders them.
        reset_all_effectives(&mut project);
        assert!(apply_step_values(&mut project), "an armed shadow reports a write");
        let value = |p: &Project| {
            p.timeline.layers[0].effects.as_ref().unwrap()[0].params.get("segs").unwrap().value
        };
        assert_eq!(value(&project), 1.0, "the step replaced the base value");

        // Transport stop (BUG-051's call site) clears the step shadow too (D4).
        clear_all_trigger_edges(&mut project);
        assert_eq!(step_value_of(&project), None, "disarm drops the shadow");
        reset_all_effectives(&mut project);
        assert!(!apply_step_values(&mut project), "no armed shadow left to apply");
        assert_eq!(value(&project), 0.0, "falls back to the committed base");
    }

    #[test]
    fn random_never_repeats_the_current_discrete_value_across_200_fires() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(&mut project, &send_id, "segs", TriggerAction::Random);

        let got = fire_n_times(&mut project, 200);
        for pair in got.windows(2) {
            assert_ne!(pair[0], pair[1], "adjacent random fires must never repeat the current value");
        }
        // And every value actually lands on a reachable discrete position.
        for v in &got {
            assert!((0.0..=8.0).contains(v) && v.fract() == 0.0, "off-grid random value {v}");
        }
    }

    #[test]
    fn random_is_deterministic_replaying_the_same_fire_sequence_twice() {
        // Two independently-built, identical projects, driven by the
        // identical fire sequence: the sequence of values must match exactly
        // both times — no RNG state, no wall clock, purely a function of the
        // mod's own fire ordinal (D7). This is the property offline export
        // leans on for reproducible audio-reactive renders.
        let (mut project_a, send_a) = project_with_audio_send();
        attach_action_mod(&mut project_a, &send_a, "segs", TriggerAction::Random);
        let (mut project_b, send_b) = project_with_audio_send();
        attach_action_mod(&mut project_b, &send_b, "segs", TriggerAction::Random);

        let seq_a = fire_n_times(&mut project_a, 50);
        let seq_b = fire_n_times(&mut project_b, 50);
        assert_eq!(seq_a, seq_b, "identical fire sequence must reproduce identical values");
    }

    #[test]
    fn action_field_serde_round_trip_old_and_new_projects_byte_identical() {
        // An old-project mod (no `action` key at all) must deserialize to
        // `TriggerAction::Continuous` and re-serialize WITHOUT ever emitting
        // an `action` key — old projects stay byte-identical on disk.
        let old_project_json = r#"{
            "paramId": "amount",
            "enabled": true,
            "source": { "sendId": "e14b42f8", "feature": { "kind": "amplitude", "band": "full" } },
            "shape": {
                "sensitivity": 1.0,
                "attackMs": 5.0,
                "releaseMs": 120.0,
                "rangeMin": 0.0,
                "rangeMax": 1.0,
                "curve": "linear",
                "invert": false,
                "rateOfChange": false
            }
        }"#;
        let m: ParameterAudioMod = serde_json::from_str(old_project_json).unwrap();
        assert_eq!(m.action, TriggerAction::Continuous);
        let reserialized = serde_json::to_string(&m).unwrap();
        assert!(!reserialized.contains("\"action\""), "Continuous must not round-trip onto the wire");
        assert!(!reserialized.contains("step_value") && !reserialized.contains("stepValue"));
        assert!(!reserialized.contains("step_dir") && !reserialized.contains("stepDir"));

        // A mod with `action` explicitly set DOES round-trip through it.
        let mut stepped = m.clone();
        stepped.action = TriggerAction::Step { amount: 2.0, wrap: WrapMode::Bounce };
        let json = serde_json::to_string(&stepped).unwrap();
        assert!(json.contains("\"action\""));
        let back: ParameterAudioMod = serde_json::from_str(&json).unwrap();
        assert_eq!(back.action, TriggerAction::Step { amount: 2.0, wrap: WrapMode::Bounce });
        // Runtime shadow state never round-trips even when `action` is set.
        assert_eq!(back.step_value, None);
        assert_eq!(back.step_dir, 1.0);
    }

    // ── PARAM_STEP_ACTIONS P2: clip-edge source + mode gating ────────────
    //
    // Pure gating-logic tests: `clip_edge_layers` is passed by hand (not
    // produced by a real `PlaybackEngine`), isolating the D3 mode-gate
    // arithmetic in `evaluate_instance_audio_mods`'s Step/Random arm from
    // the engine's own edge-production mechanism (that end-to-end path is
    // covered separately in
    // `crates/manifold-playback/tests/param_step_clip_edge.rs`, which
    // exercises the real `sync_clips_to_time` → `last_active_clip_id` →
    // `clip_edge_layers` pipeline through `PlaybackEngine::tick`).
    // `snapshot_full_transient(0.0)` never crosses the 0.5 edge threshold,
    // so every fire observed here is attributable purely to the clip-edge
    // source, not the mod's own audio edge.

    #[test]
    fn clip_edge_mode_fires_from_clip_edge_alone_no_audio_signal() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .trigger_mode = Some(TriggerFireMode::ClipEdge);

        let cold = snapshot_full_transient(0.0);
        // Step/Random never set the `wrote` return bool (D4: the fire only
        // advances the runtime shadow; `p.value` is written by Phase 1.5 on
        // the NEXT tick) — assert on `step_value`, not the return value,
        // matching every P1 step test's convention.
        evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[0], &mut FireMeterCapture::default());
        assert_eq!(
            step_value_of(&project),
            Some(1.0),
            "ClipEdge mode fires purely from the layer's clip edge"
        );
    }

    #[test]
    fn transient_mode_step_ignores_clip_edge() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .trigger_mode = Some(TriggerFireMode::Transient);

        let cold = snapshot_full_transient(0.0);
        assert!(!evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[0], &mut FireMeterCapture::default()));
        assert_eq!(step_value_of(&project), None, "Transient mode never reacts to a clip edge");
    }

    #[test]
    fn none_trigger_mode_on_step_defaults_to_transient_not_both() {
        // D3: unlike gate cards (default Both), a step mod's unset
        // `trigger_mode` defaults to Transient — a clip edge alone must NOT
        // fire it (arming a step required opening an audio drawer, so a
        // config with no audio intent at all is meaningless).
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );
        // trigger_mode left at attach_action_mod's default: None.

        let cold = snapshot_full_transient(0.0);
        assert!(!evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[0], &mut FireMeterCapture::default()));
        assert_eq!(
            step_value_of(&project),
            None,
            "unset trigger_mode on a step defaults to Transient, not Both"
        );
    }

    #[test]
    fn both_mode_sums_audio_and_clip_edge_sources() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .trigger_mode = Some(TriggerFireMode::Both);

        // Clip edge alone (no audio signal) fires once.
        let cold = snapshot_full_transient(0.0);
        evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[0], &mut FireMeterCapture::default());
        assert_eq!(step_value_of(&project), Some(1.0));

        // A hot audio signal with NO clip edge this time also fires — Both
        // sums the two sources rather than requiring either exclusively.
        let hot = snapshot_full_transient(0.9);
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        assert_eq!(
            step_value_of(&project),
            Some(2.0),
            "the audio edge alone also fires under Both, summing with the earlier clip-edge fire"
        );
    }

    #[test]
    fn clip_edge_on_a_different_layer_index_does_not_fire() {
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp },
        );
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .trigger_mode = Some(TriggerFireMode::ClipEdge);

        let cold = snapshot_full_transient(0.0);
        // The edge is reported for layer 7 — this mod lives on layer 0.
        assert!(!evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[7], &mut FireMeterCapture::default()));
        assert_eq!(
            step_value_of(&project),
            None,
            "a clip edge on an unrelated layer index must not fire this layer's step"
        );
    }

    #[test]
    fn master_chain_mod_never_sees_a_clip_edge_even_when_layer_0_has_one() {
        // D3/D5: master-chain instances have no layer, so their clip
        // contribution is always 0 — even when `clip_edge_layers` happens to
        // contain layer index 0 (a real per-layer instance's index), a
        // master-effect's own mod must not alias onto it.
        let mut project = Project::default();
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);
        let mut fx = create_default(&TEST_FX);
        let mut m = ParameterAudioMod::new(
            "segs".into(),
            send_id.clone(),
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.shape = AudioModShape { attack_ms: 0.0, release_ms: 0.0, ..Default::default() };
        m.action = TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp };
        m.trigger_mode = Some(TriggerFireMode::ClipEdge);
        fx.audio_mods_mut().push(m);
        project.settings.master_effects.push(fx);

        let cold = snapshot_full_transient(0.0);
        assert!(!evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[0], &mut FireMeterCapture::default()));
        let step = project.settings.master_effects[0].audio_mods.as_ref().unwrap()[0].step_value;
        assert_eq!(
            step, None,
            "master-chain instances never see a clip edge, regardless of clip_edge_layers contents"
        );
    }

    // ── trim-handle zone rails (BUG class-kill): detection reads the
    //    conditioned (pre-range-map) signal; Step/Random travel within the
    //    zone the handles define, not the param's full min/max ──────────────

    #[test]
    fn step_fires_repeatedly_when_range_min_would_have_killed_detection() {
        // OLD behavior: edge detection ran on the range-mapped signal. With
        // range_min = 0.6, the mapped floor never drops below 0.6 (>= the 0.5
        // threshold), so the mod fires once and never re-arms. Detection must
        // run on the conditioned (pre-map) signal instead, so the mod keeps
        // firing on every repeated hit exactly like the default-handle case.
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Wrap },
        );
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .shape
            .range_min = 0.6;

        let got = fire_n_times(&mut project, 5);
        for (i, pair) in got.windows(2).enumerate() {
            assert_ne!(
                pair[0], pair[1],
                "fire {i}->{}: mod must re-arm and step every hit even with range_min=0.6, got {:?}",
                i + 1,
                got
            );
        }
    }

    #[test]
    fn step_fires_repeatedly_when_range_max_would_have_killed_detection() {
        // OLD behavior: with range_max = 0.4, the mapped ceiling never rises
        // above 0.4 (< the 0.5 threshold), so the mod NEVER fires at all.
        // Detection on the conditioned signal must fire normally.
        let (mut project, send_id) = project_with_audio_send();
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Wrap },
        );
        project.timeline.layers[0].effects.as_mut().unwrap()[0]
            .audio_mods
            .as_mut()
            .unwrap()[0]
            .shape
            .range_max = 0.4;

        let got = fire_n_times(&mut project, 5);
        for (i, pair) in got.windows(2).enumerate() {
            assert_ne!(
                pair[0], pair[1],
                "fire {i}->{}: mod must fire every hit even with range_max=0.4, got {:?}",
                i + 1,
                got
            );
        }
    }

    #[test]
    fn step_wrap_respects_the_trim_handle_zone_not_the_full_param_range() {
        // Continuous param 0..10 (TestEnvGen's "speed"). Handles 0.2/0.8 map
        // to rails 2..8. Wrap from a seeded current of 7 with amount 2 must
        // land at 3 (wraps past 8, not past the param's true max of 10).
        let layer = generator_layer();
        let mut project = project_with(layer);
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);

        let mut m = ParameterAudioMod::new(
            "speed".into(),
            send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.shape = AudioModShape {
            attack_ms: 0.0,
            release_ms: 0.0,
            range_min: 0.2,
            range_max: 0.8,
            ..Default::default()
        };
        m.action = TriggerAction::Step { amount: 2.0, wrap: WrapMode::Wrap };
        m.step_value = Some(7.0); // seed the current directly inside the zone
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(m);

        let hot = snapshot_full_transient(0.9);
        evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());

        let step = project.timeline.layers[0]
            .gen_params()
            .unwrap()
            .audio_mods
            .as_ref()
            .unwrap()[0]
            .step_value;
        assert_eq!(
            step,
            Some(3.0),
            "zone 2..8 (handles 0.2/0.8 on speed's 0..10 range): 7 + 2 wraps past 8 to 3"
        );
    }

    #[test]
    fn random_stays_inside_the_trim_handle_zone_across_many_fires() {
        // Continuous param 0..10, handles 0.2/0.8 -> zone 2..8. Every random
        // step_value across many fires must stay within the zone, never
        // reaching into the untrimmed 0..2 or 8..10 slivers.
        let layer = generator_layer();
        let mut project = project_with(layer);
        let send = AudioSend::new("Kick");
        let send_id = send.id.clone();
        project.audio_setup.sends.push(send);

        let mut m = ParameterAudioMod::new(
            "speed".into(),
            send_id,
            AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
        );
        m.shape = AudioModShape {
            attack_ms: 0.0,
            release_ms: 0.0,
            range_min: 0.2,
            range_max: 0.8,
            ..Default::default()
        };
        m.action = TriggerAction::Random;
        project.timeline.layers[0]
            .gen_params_mut()
            .unwrap()
            .audio_mods_mut()
            .push(m);

        let hot = snapshot_full_transient(0.9);
        let cold = snapshot_full_transient(0.0);
        for _ in 0..100 {
            evaluate_all_audio_mods(&mut project, &hot, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
            let step = project.timeline.layers[0]
                .gen_params()
                .unwrap()
                .audio_mods
                .as_ref()
                .unwrap()[0]
                .step_value;
            if let Some(v) = step {
                assert!((2.0..=8.0).contains(&v), "random value {v} escaped the zone 2..8");
            }
            evaluate_all_audio_mods(&mut project, &cold, Seconds(0.016), &mut Vec::new(), &[], &mut FireMeterCapture::default());
        }
    }

    #[test]
    fn discrete_zone_random_only_produces_the_snapped_integer_rails() {
        // "segs" recalibrated to 0..9 (whole numbers). Handles 0.25/0.75 snap
        // to the integer rails 3..6 (zone() gives 2.25..6.75, ceil/floor snaps
        // to 3..6). Random must only ever land on {3,4,5,6} and never repeat
        // the current value back-to-back.
        let (mut project, send_id) = project_with_audio_send();
        override_stock_param_range(
            &mut project.timeline.layers[0].effects.as_mut().unwrap()[0],
            "segs",
            0.0,
            9.0,
        );
        attach_action_mod(&mut project, &send_id, "segs", TriggerAction::Random);
        {
            let m = &mut project.timeline.layers[0].effects.as_mut().unwrap()[0]
                .audio_mods
                .as_mut()
                .unwrap()[0];
            m.shape.range_min = 0.25;
            m.shape.range_max = 0.75;
        }

        let got = fire_n_times(&mut project, 200);
        for v in &got {
            assert!(
                [3.0, 4.0, 5.0, 6.0].contains(v),
                "random must stay within the snapped integer rails 3..6, got {v}"
            );
        }
        for pair in got.windows(2) {
            assert_ne!(pair[0], pair[1], "adjacent random fires must never repeat the current value");
        }
    }

    #[test]
    fn discrete_zone_step_wrap_cycles_within_the_snapped_integer_rails() {
        // Same recalibrated "segs" (0..9) + handles 0.25/0.75 -> rails 3..6.
        // Step Wrap from the committed base (0, clamped into the zone at 3)
        // cycles 3 -> 4 -> 5 -> 6 -> 3, never touching 0, 1, 2, 7, 8, or 9.
        let (mut project, send_id) = project_with_audio_send();
        override_stock_param_range(
            &mut project.timeline.layers[0].effects.as_mut().unwrap()[0],
            "segs",
            0.0,
            9.0,
        );
        attach_action_mod(
            &mut project,
            &send_id,
            "segs",
            TriggerAction::Step { amount: 1.0, wrap: WrapMode::Wrap },
        );
        {
            let m = &mut project.timeline.layers[0].effects.as_mut().unwrap()[0]
                .audio_mods
                .as_mut()
                .unwrap()[0];
            m.shape.range_min = 0.25;
            m.shape.range_max = 0.75;
        }

        let expect = [4.0, 5.0, 6.0, 3.0];
        let got = fire_n_times(&mut project, expect.len());
        assert_eq!(got, expect, "wrap cycles 4,5,6,3 within the snapped integer rails 3..6, got {got:?}");
    }

    // ── BUG-039: driver (LFO) wrap ──────────────────────────────────────

    /// A Sawtooth driver whose trim overshoots the param's own range (a
    /// performer dragging a trim handle past 100%, or a driver ported from a
    /// wider-range project) must wrap a periodic param back into range
    /// instead of leaving it dangling past `max`.
    #[test]
    fn driver_wraps_periodic_param_past_trim_overshoot() {
        let layer = layer_with_one_effect();
        let mut project = project_with(layer);
        {
            let fx = &mut project.timeline.layers[0].effects.as_mut().unwrap()[0];
            override_stock_param_range(fx, "amount", 0.0, 1.0);
            fx.params.get_mut("amount").unwrap().spec.wraps = true;
            let mut driver = ParameterDriver::new("amount", BeatDivision::Whole, DriverWaveform::Sawtooth);
            driver.free_period_beats = Some(1.0); // 1-beat period so phase == beat directly
            driver.trim_min = 0.0;
            driver.trim_max = 1.5; // 50% overshoot past the param's own max
            fx.drivers = Some(vec![driver]);
        }

        // Sawtooth phase 0.8 at beat 0.8 of a 1-beat period -> raw target
        // 1.5*0.8 = 1.2, past max 1.0. Wrapped: 0.0 + (1.2-0.0).rem_euclid(1.0) = 0.2.
        evaluate_instance_drivers(
            &mut project.timeline.layers[0].effects.as_mut().unwrap()[0],
            Beats(0.8),
        );
        let value = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap()
            .value;
        assert!(
            (value - 0.2).abs() < 1e-5,
            "overshooting trim must wrap back into [0,1], got {value}"
        );
        assert!((0.0..1.0).contains(&value), "wrapped value must stay in range, got {value}");
    }

    /// The same overshoot on a NON-periodic param (default `wraps: false`,
    /// e.g. FOV) must still clamp at the rail — wrapping is opt-in per
    /// param, never the default.
    #[test]
    fn driver_clamps_non_periodic_param_past_trim_overshoot() {
        let layer = layer_with_one_effect();
        let mut project = project_with(layer);
        {
            let fx = &mut project.timeline.layers[0].effects.as_mut().unwrap()[0];
            override_stock_param_range(fx, "amount", 0.0, 1.0);
            // wraps stays false (default) — the FOV-style case.
            let mut driver = ParameterDriver::new("amount", BeatDivision::Whole, DriverWaveform::Sawtooth);
            driver.free_period_beats = Some(1.0); // 1-beat period so phase == beat directly
            driver.trim_min = 0.0;
            driver.trim_max = 1.5;
            fx.drivers = Some(vec![driver]);
        }

        evaluate_instance_drivers(
            &mut project.timeline.layers[0].effects.as_mut().unwrap()[0],
            Beats(0.8),
        );
        let value = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap()
            .value;
        assert!(
            (value - 1.0).abs() < 1e-6,
            "non-wrapping param must clamp at max, got {value}"
        );
    }

    /// The full-cycle regression gate: a saw LFO driving a `wraps: true`
    /// param across many periods never plateaus at the rail (the pre-fix
    /// symptom class) — every sampled value stays strictly inside
    /// `[min, max)`, and the sequence is monotonic-with-resets (rises, then
    /// drops back to ~0 at each period boundary) rather than climbing past
    /// max and sticking there.
    #[test]
    fn driver_saw_sweep_never_plateaus_across_multiple_periods() {
        let layer = layer_with_one_effect();
        let mut project = project_with(layer);
        {
            let fx = &mut project.timeline.layers[0].effects.as_mut().unwrap()[0];
            override_stock_param_range(fx, "amount", 0.0, 360.0);
            fx.params.get_mut("amount").unwrap().spec.wraps = true;
            let mut driver = ParameterDriver::new("amount", BeatDivision::Whole, DriverWaveform::Sawtooth);
            driver.free_period_beats = Some(1.0); // 1-beat period so phase == beat directly
            fx.drivers = Some(vec![driver]);
        }

        let mut samples = Vec::new();
        for i in 0..400 {
            let beat = Beats(i as f64 * 0.01); // 4 full periods at 100 samples/period
            evaluate_instance_drivers(
                &mut project.timeline.layers[0].effects.as_mut().unwrap()[0],
                beat,
            );
            let value = project.timeline.layers[0].effects.as_ref().unwrap()[0]
                .params
                .get("amount")
                .unwrap()
                .value;
            assert!(
                (0.0..360.0).contains(&value),
                "sample at beat {beat:?} out of [0,360): {value}"
            );
            samples.push(value);
        }
        // Never two consecutive frames sitting at (near-)max — a plateau is
        // exactly the symptom BUG-039 fixed: the value getting stuck at the
        // rail instead of resetting to ~0 at the wrap.
        for w in samples.windows(2) {
            let stuck_at_top = w[0] > 355.0 && w[1] > 355.0;
            assert!(!stuck_at_top, "value plateaued near max instead of wrapping: {w:?}");
        }
    }

    // ── UX-P3a (SCENE_PANEL_UX_DESIGN.md D8): exposure-on-demand → modulation ──
    //
    // The scene panel's mod button (`crates/manifold-ui/src/panels/
    // scene_setup_panel.rs`) never targets the modulation pipeline directly —
    // it dispatches the SAME `ToggleNodeParamExposeCommand` the graph editor's
    // expose glyph uses, which mints a `UserParamBinding` (and its
    // `ParamManifest` slot) on the layer's `gen_params`. Everything downstream
    // — this file's `evaluate_all_drivers` included — is the existing system,
    // untouched by P3a (D8's own claim). This test proves that claim isn't
    // just asserted, it's true: expose an inner node's param through the real
    // command, attach a driver to the id that command minted, and confirm
    // `evaluate_all_drivers` moves it — the exact mechanism a scene row's mod
    // button reaches, end to end, without needing the live UI/headless-render
    // harness (which doesn't run a content-thread tick, so it can't observe
    // per-frame driver output — see `scripts/ui-flows/
    // scene-panel-ux-p3a-expose-modulate.json`'s own doc note).
    #[test]
    fn exposed_generator_param_is_driver_modulated_across_beats() {
        use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
        use manifold_core::effects::ParamConvert;
        use manifold_core::{GraphTarget, NodeId};
        use manifold_editing::command::Command;
        use manifold_editing::commands::graph::ToggleNodeParamExposeCommand;
        use std::collections::BTreeMap;

        // Minimal generator graph: one node with a "roughness" param, a
        // stable `node_id` and `handle` (exposure needs both — see
        // `ToggleNodeParamExposeCommand`'s own doc comment on why a handle
        // is required to mint a readable user-param id).
        let mut params = BTreeMap::new();
        params.insert("roughness".to_string(), SerializedParamValue::Float { value: 0.3 });
        let node = EffectGraphNode {
            id: 10,
            node_id: NodeId::new("mat_0"),
            type_id: "node.pbr_material".to_string(),
            handle: Some("mat_0".to_string()),
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![node],
            wires: vec![],
        };

        let mut layer = generator_layer();
        layer.gen_params_or_init().graph = Some(def.clone());
        let mut project = project_with(layer);
        let layer_id = project.timeline.layers[0].layer_id.clone();

        // The exact command the scene panel's mod button dispatches
        // (`PanelAction::SceneSetupExposeParam`'s app-side handler,
        // `ui_bridge/project.rs`).
        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(layer_id),
            NodeId::new("mat_0"),
            10,
            "mat_0".to_string(),
            "roughness".to_string(),
            true,
            def,
            "QS1694-W02-1-1 \u{b7} Roughness".to_string(),
            0.01,
            1.0,
            0.3,
            ParamConvert::Float,
            false,
            Vec::new(),
        );
        cmd.execute(&mut project);

        let gp = project.timeline.layers[0].gen_params().unwrap();
        let bindings = gp.user_param_bindings();
        assert_eq!(bindings.len(), 1, "expose must mint exactly one user param binding");
        let user_param_id = bindings[0].id.clone();
        assert!(
            user_param_id.starts_with("user.mat_0.roughness"),
            "id must be minted from the exposed node handle + param: {user_param_id}"
        );
        assert_eq!(bindings[0].label, "QS1694-W02-1-1 \u{b7} Roughness");
        assert!(
            gp.params.get(&user_param_id).is_some(),
            "append_user_binding must grow the manifest with a slot for the new id"
        );

        // Attach a driver to the SAME id the expose command minted — the
        // scene row's card, one click later in the real app.
        let driver = manifold_core::effects::ParameterDriver {
            param_id: user_param_id.clone().into(),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.3,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            free_period_beats: None,
            legacy_param_index: None,
            is_paused_by_user: false,
        };
        project.timeline.layers[0].gen_params_mut().unwrap().drivers = Some(vec![driver]);

        // Quarter-note period (1 beat): beat 0.25 sits at the sine's quarter
        // phase (peak), beat 0 at its zero-crossing (midpoint) — a beat pair
        // guaranteed apart by symmetry, unlike e.g. 0 vs 0.5 (both land on a
        // zero-crossing of a period-1 sine and alias to the same value).
        assert!(evaluate_all_drivers(&mut project, Beats(0.0)), "driver must report itself active");
        let v0 = project.timeline.layers[0].gen_params().unwrap().params.get(&user_param_id).unwrap().value;
        evaluate_all_drivers(&mut project, Beats(0.25));
        let v1 = project.timeline.layers[0].gen_params().unwrap().params.get(&user_param_id).unwrap().value;

        assert!(
            (v0 - v1).abs() > 0.05,
            "a driver on the exposed param must move its value across beats: {v0} at beat 0, {v1} at beat 0.25"
        );
    }

    /// UX-P3a's round-trip gate (BUG-036 rule): save → reload → the exposed
    /// scene param still modulates AFTER reload, asserted — not assumed. The
    /// param manifest's deserialize-then-reconcile split (BUG-036's own
    /// class) is exactly the kind of thing that silently drops a freshly-
    /// appended `UserParamBinding`'s manifest slot; round-tripping through
    /// the REAL `serde_json` + `load_project_from_json` path (same migration
    /// + reconcile pass a `.manifold` file goes through, per
    ///   `legacy_route_migrates_and_the_round_trip_survives_save_and_reload`'s
    ///   precedent in `manifold-io`) is the only honest way to prove it.
    #[test]
    fn exposed_generator_param_survives_save_reload_and_still_modulates() {
        use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
        use manifold_core::effects::ParamConvert;
        use manifold_core::{GraphTarget, NodeId};
        use manifold_editing::command::Command;
        use manifold_editing::commands::graph::ToggleNodeParamExposeCommand;
        use std::collections::BTreeMap;

        let mut params = BTreeMap::new();
        params.insert("roughness".to_string(), SerializedParamValue::Float { value: 0.3 });
        let node = EffectGraphNode {
            id: 10,
            node_id: NodeId::new("mat_0"),
            type_id: "node.pbr_material".to_string(),
            handle: Some("mat_0".to_string()),
            params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![node],
            wires: vec![],
        };

        let mut layer = generator_layer();
        layer.gen_params_or_init().graph = Some(def.clone());
        let mut project = project_with(layer);
        let layer_id = project.timeline.layers[0].layer_id.clone();

        let mut cmd = ToggleNodeParamExposeCommand::new(
            GraphTarget::Generator(layer_id),
            NodeId::new("mat_0"),
            10,
            "mat_0".to_string(),
            "roughness".to_string(),
            true,
            def,
            "QS1694-W02-1-1 \u{b7} Roughness".to_string(),
            0.01,
            1.0,
            0.3,
            ParamConvert::Float,
            false,
            Vec::new(),
        );
        cmd.execute(&mut project);
        let user_param_id =
            project.timeline.layers[0].gen_params().unwrap().user_param_bindings()[0].id.clone();

        project.timeline.layers[0].gen_params_mut().unwrap().drivers = Some(vec![
            manifold_core::effects::ParameterDriver {
                param_id: user_param_id.clone().into(),
                beat_division: BeatDivision::Quarter,
                waveform: DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.3,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            },
        ]);

        // ── Save → reload through the REAL serde + migrate/reconcile path ──
        let json = serde_json::to_string(&project).expect("project serializes");
        let mut reloaded =
            manifold_io::loader::load_project_from_json(&json).expect("reloaded project parses");

        let gp = reloaded.timeline.layers[0].gen_params().expect("gen_params survives reload");
        let bindings = gp.user_param_bindings();
        assert_eq!(bindings.len(), 1, "the exposed binding survives reload");
        assert_eq!(bindings[0].id, user_param_id, "same stable id after reload");
        assert_eq!(
            bindings[0].label, "QS1694-W02-1-1 \u{b7} Roughness",
            "the <ObjectName> · <ParamLabel> name survives reload"
        );
        assert!(
            gp.params.get(&user_param_id).is_some(),
            "BUG-036 class check: the manifest slot for the reloaded binding must exist, \
             not just the binding record — a partial reconcile silently drops this"
        );
        assert_eq!(
            gp.drivers.as_ref().map(|d: &Vec<_>| d.len()),
            Some(1),
            "the driver targeting the exposed param survives reload"
        );

        // ── Still modulates AFTER reload — not assumed. ──
        assert!(
            evaluate_all_drivers(&mut reloaded, Beats(0.0)),
            "driver must still report itself active after reload"
        );
        let v0 = reloaded.timeline.layers[0].gen_params().unwrap().params.get(&user_param_id).unwrap().value;
        evaluate_all_drivers(&mut reloaded, Beats(0.25));
        let v1 = reloaded.timeline.layers[0].gen_params().unwrap().params.get(&user_param_id).unwrap().value;
        assert!(
            (v0 - v1).abs() > 0.05,
            "the reloaded driver must still move the exposed param's value: {v0} at beat 0, {v1} at beat 0.25"
        );
    }
}
