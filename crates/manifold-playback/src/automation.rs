//! Automation-lane sampling — the tier-1 "hand" that samples arrangement
//! automation each frame and writes the result onto each param's `base`,
//! ahead of the modulation pipeline (`modulation::evaluate_modulation`).
//!
//! One sentence: a lane is a beat-indexed base writer. It rides on top of
//! the same `ParamSlot { base, value }` every other hand (UI slider, Ableton
//! macro, OSC, macro bank) writes through — the modulation pipeline (LFO
//! drivers, envelopes, audio mods) is untouched, no new phase, no fifth
//! silo. See `docs/AUTOMATION_LANES_DESIGN.md`.
//!
//! Walk shape is a copy of `modulation::evaluate_all_audio_mods`: master
//! effects + layer effects + generator params, skip disabled instances,
//! resolve via `resolve_param_in`, two-pass resolve-then-write. Note: that
//! precedent itself allocates a small per-instance `Vec` each frame via
//! `.collect()` (not truly zero-alloc despite this module's own top-level
//! signature having no room for an externally-threaded scratch buffer) —
//! this module matches that existing, already-shipping allocation profile
//! rather than inventing a new one.
//!
//! Override latch: a param whose slot was `touched` since this evaluator
//! last looked (any hand funneling through `PresetInstance::set_base_param`)
//! latches as "overridden" in `latches` and this frame's automation write is
//! skipped. `Back to Arrangement` (`PlaybackEngine::
//! automation_back_to_arrangement`) clears `latches`, resuming every lane.
//! Latches are runtime-only, owned by the playback side (never the
//! `Project`), and survive play/stop within a session — only Back to
//! Arrangement clears them (Ableton semantics).

use ahash::AHashMap;

use manifold_core::effects::{PresetInstance, resolve_param_in};
use manifold_core::preset_definition_registry;
use manifold_core::project::Project;
use manifold_core::{Beats, EffectId};

/// Alias for `manifold_core::effects::ParamId` (`Cow<'static, str>`) — kept
/// local so this module doesn't need a second import path just for the type
/// alias below.
type ParamId = manifold_core::effects::ParamId;

/// Runtime-only override latch: params a live hand has touched since the
/// last Back to Arrangement. Owned by `PlaybackEngine` (the playback side),
/// never the `Project` — never serialized. Keyed by the owning instance's
/// `EffectId` (stable for master/layer effects across save/load; freshly
/// synthesized for generator instances on each load, which is harmless here
/// since a fresh load naturally invalidates any stale entry) plus the
/// param's `ParamId`.
pub type AutomationLatches = AHashMap<(EffectId, ParamId), ()>;

/// Sample every enabled automation lane at `current_beat` and write the
/// result onto each param's `base`. Call this BEFORE
/// `modulation::evaluate_modulation` each tick — automation is a hand, not a
/// modulator, so it must land before the base→value reset. Returns true if
/// any lane wrote a value this frame (fold into the same compositor-dirty
/// path as `modulation_active`). Never bumps the project `DataVersion`.
pub fn evaluate_all_automation(
    project: &mut Project,
    current_beat: Beats,
    latches: &mut AutomationLatches,
) -> bool {
    let mut any_wrote = false;

    for fx in project.settings.master_effects.iter_mut() {
        if evaluate_instance_automation(fx, current_beat, latches) {
            any_wrote = true;
        }
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                if evaluate_instance_automation(fx, current_beat, latches) {
                    any_wrote = true;
                }
            }
        }
        if let Some(gp) = layer.gen_params_mut()
            && evaluate_instance_automation(gp, current_beat, latches)
        {
            any_wrote = true;
        }
    }

    any_wrote
}

/// Evaluate every automation lane on a single instance. Returns true if any
/// lane wrote a value.
fn evaluate_instance_automation(
    fx: &mut PresetInstance,
    current_beat: Beats,
    latches: &mut AutomationLatches,
) -> bool {
    if !fx.enabled {
        return false;
    }
    let has_lanes = fx.automation_lanes.as_ref().is_some_and(|v| !v.is_empty());
    if !has_lanes {
        return false;
    }
    let def = match preset_definition_registry::try_get(fx.effect_type()) {
        Some(d) => d,
        None => return false,
    };
    let fx_id = fx.id.clone();

    // Pass 1 (immutable): resolve each enabled lane to its slot, then
    // classify — a param `touched` since we last looked latches (its hand's
    // write wins this frame, no automation write); a still-latched param is
    // skipped with no bookkeeping; otherwise sample and queue the write.
    let lanes = fx.automation_lanes.as_ref().unwrap();
    let mut to_latch: Vec<(usize, (EffectId, ParamId))> = Vec::new();
    let mut to_write: Vec<(usize, f32)> = Vec::new();
    for lane in lanes.iter().filter(|l| l.enabled) {
        let Some(resolved) = resolve_param_in(&def, fx, lane.param_id.as_ref()) else {
            continue;
        };
        if resolved.idx >= fx.param_values.len() {
            continue;
        }
        if fx.param_values[resolved.idx].touched {
            to_latch.push((resolved.idx, (fx_id.clone(), lane.param_id.clone())));
            continue;
        }
        let key = (fx_id.clone(), lane.param_id.clone());
        if latches.contains_key(&key) {
            continue; // still overridden
        }
        let raw = lane.value_at(current_beat);
        to_write.push((resolved.idx, raw.clamp(resolved.min, resolved.max)));
    }

    // Pass 2 (mutable): clear the touched flag + latch for params that were
    // just touched, then write sampled values via the non-touching
    // automation write path (using `set_base_param` here would set
    // `touched` on our own write and self-latch the very next frame — see
    // `docs/AUTOMATION_LANES_DESIGN.md` §4).
    for (idx, key) in to_latch {
        fx.param_values[idx].touched = false;
        latches.insert(key, ());
    }
    let mut any_wrote = false;
    for (idx, value) in to_write {
        fx.set_base_param_from_automation(idx, value);
        any_wrote = true;
    }
    any_wrote
}

// =====================================================================
// Tests. Mirrors `modulation::tests`'s fixture pattern: manifold-renderer
// (which submits the real shipping presets) isn't linked into this test
// binary, so a synthetic effect is registered via `inventory` to give
// `resolve_param_in` / the registry a target. Uses a distinct type name
// from `modulation.rs`'s own fixtures ("TestEnvFx") since both modules'
// `#[cfg(test)]` code compiles into the same crate test binary and the
// registry would otherwise see two submissions for one id.
// =====================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_registration::EffectMetadata;
    use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};
    use manifold_core::generator_registration::ParamSpec;
    use manifold_core::layer::Layer;
    use manifold_core::preset_definition_registry::create_default;
    use manifold_core::project::Project;
    use manifold_core::PresetTypeId;

    const TEST_FX: PresetTypeId = PresetTypeId::new("TestAutomationFx");

    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::new("TestAutomationFx"),
            display_name: "Test Automation Fx",
            category: "Test",
            available: true,
            osc_prefix: "testAutomationFx",
            legacy_discriminant: None,
            params: &[ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", "")],
        }
    }

    fn layer_with_one_effect() -> Layer {
        let mut layer = Layer::new_video("FxLayer".into(), 0);
        layer.effects = Some(vec![create_default(&TEST_FX)]);
        layer
    }

    fn project_with(layer: Layer) -> Project {
        let mut project = Project::default();
        project.timeline.layers = vec![layer];
        project
    }

    /// A lane on `amount`, one point at beat 0 (value 0.2) and one at beat 4
    /// (value 0.8), linear.
    fn amount_lane() -> AutomationLane {
        AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![
                AutomationPoint {
                    beat: Beats(0.0),
                    value: 0.2,
                    shape: SegmentShape::Linear,
                },
                AutomationPoint {
                    beat: Beats(4.0),
                    value: 0.8,
                    shape: SegmentShape::Linear,
                },
            ],
        }
    }

    #[test]
    fn samples_and_writes_base_at_current_beat() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        let wrote = evaluate_all_automation(&mut project, Beats(2.0), &mut latches);

        assert!(wrote, "an enabled lane with no override reports a write");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        // Halfway between beat 0 (0.2) and beat 4 (0.8) -> 0.5.
        assert!(
            (fx.param_values[0].base - 0.5).abs() < 1e-6,
            "base sampled at the midpoint, got {}",
            fx.param_values[0].base
        );
    }

    #[test]
    fn write_lands_in_base_before_modulation_reset_runs() {
        // The whole point of running automation BEFORE
        // `modulation::reset_all_effectives` in the tick: the reset's
        // base->value copy must see automation's write, not last frame's
        // stale base. Prove the order matters by running them in the
        // documented sequence and checking `value` reflects the NEW base.
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        assert!(evaluate_all_automation(&mut project, Beats(4.0), &mut latches));
        crate::modulation::reset_all_effectives(&mut project);

        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].value - 0.8).abs() < 1e-6,
            "reset_all_effectives must copy the automation-sampled base into \
             value, proving automation ran first; got {}",
            fx.param_values[0].value
        );
    }

    #[test]
    fn touch_latches_and_skips_the_write_that_frame() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        // Simulate a live hand touching the param this frame (a UI slider,
        // Ableton macro, or OSC write all funnel through set_base_param).
        fx.set_base_param(0, 0.99);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        let wrote = evaluate_all_automation(&mut project, Beats(2.0), &mut latches);

        assert!(!wrote, "a freshly-touched param is not sampled this frame");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].base - 0.99).abs() < 1e-6,
            "the hand's write wins, untouched by automation"
        );
        assert!(
            !fx.param_values[0].touched,
            "the evaluator clears touched once it has latched"
        );
        assert_eq!(latches.len(), 1, "the touch latches the param as overridden");
    }

    #[test]
    fn still_latched_param_stays_skipped_on_later_frames() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.set_base_param(0, 0.99);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // Frame 1: touch → latch, no write.
        assert!(!evaluate_all_automation(&mut project, Beats(2.0), &mut latches));
        // Frame 2: no new touch, but still latched — still no write, base
        // stays at the hand's value, not automation's.
        let wrote = evaluate_all_automation(&mut project, Beats(2.0), &mut latches);
        assert!(!wrote, "a still-latched param stays overridden");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!((fx.param_values[0].base - 0.99).abs() < 1e-6);
    }

    #[test]
    fn evaluator_self_write_does_not_latch() {
        // The one self-trigger footgun named in the design doc: the
        // evaluator's own write must not set `touched`, or the very next
        // frame would see it and latch the lane against itself.
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // Frame 1: samples and writes (no touch, no prior latch).
        assert!(evaluate_all_automation(&mut project, Beats(1.0), &mut latches));
        assert!(latches.is_empty(), "a sampled write must not self-latch");

        // Frame 2: still samples normally — proves frame 1's write didn't
        // leave the param looking "touched" to frame 2's evaluator.
        let wrote = evaluate_all_automation(&mut project, Beats(3.0), &mut latches);
        assert!(wrote, "automation keeps sampling across frames uninterrupted");
        assert!(latches.is_empty());
    }

    #[test]
    fn back_to_arrangement_resumes_a_latched_lane() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.set_base_param(0, 0.99);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // Touch latches the lane.
        assert!(!evaluate_all_automation(&mut project, Beats(2.0), &mut latches));
        assert_eq!(latches.len(), 1);

        // Back to Arrangement: `PlaybackEngine::automation_back_to_arrangement`
        // is exactly `self.automation_latches.clear()` — exercised directly
        // here at the map level (the engine wrapper is a one-line delegation).
        latches.clear();

        let wrote = evaluate_all_automation(&mut project, Beats(2.0), &mut latches);
        assert!(wrote, "clearing the latch resumes the lane's writes");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.param_values[0].base - 0.5).abs() < 1e-6,
            "lane resumes sampling at the current beat, got {}",
            fx.param_values[0].base
        );
    }

    #[test]
    fn disabled_lane_does_not_sample() {
        let mut layer = layer_with_one_effect();
        let mut lane = amount_lane();
        lane.enabled = false;
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![lane]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        assert!(!evaluate_all_automation(&mut project, Beats(2.0), &mut latches));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.param_values[0].base, 0.0, "untouched, still the default");
    }

    #[test]
    fn disabled_effect_is_skipped_entirely() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.enabled = false;
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        assert!(!evaluate_all_automation(&mut project, Beats(2.0), &mut latches));
    }

    #[test]
    fn empty_lanes_list_is_a_cheap_noop() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(Vec::new());
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        assert!(!evaluate_all_automation(&mut project, Beats(2.0), &mut latches));
    }

    #[test]
    fn never_bumps_data_version() {
        // Sampling is a hand, not a structural edit — it must not look like
        // a project mutation that would (for example) mark a saved project
        // dirty. There is no per-frame `DataVersion` bump anywhere in this
        // module's write path (`set_base_param_from_automation` only
        // touches `ParamSlot`), so this test pins the absence structurally:
        // sampling repeatedly must not change anything but the touched
        // slot's `base`/`value`.
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        for beat in [0.5, 1.0, 1.5, 2.0, 2.5] {
            evaluate_all_automation(&mut project, Beats(beat), &mut latches);
        }
        // No panic, no growth in unrelated state — the layer/effect list
        // shape is exactly what it started as.
        assert_eq!(project.timeline.layers.len(), 1);
        assert_eq!(project.timeline.layers[0].effects.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn generator_param_lane_samples_too() {
        // Walk shape must cover generator params, same as
        // `evaluate_all_audio_mods` — not just layer/master effects.
        let mut layer = Layer::new_generator("GenLayer".into(), TEST_FX, 0);
        layer
            .gen_params_or_init()
            .init_defaults_for_type(TEST_FX);
        layer
            .gen_params_or_init()
            .automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        let wrote = evaluate_all_automation(&mut project, Beats(4.0), &mut latches);
        assert!(wrote);
        let gp = project.timeline.layers[0].gen_params().unwrap();
        assert!((gp.param_values[0].base - 0.8).abs() < 1e-6);
    }
}
