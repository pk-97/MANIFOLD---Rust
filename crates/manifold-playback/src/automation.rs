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
//!
//! ## Recording (§5)
//!
//! When the global Automation Arm is on, a touched param (while playing)
//! records into its lane instead of latching an override — the SAME
//! touched-flag funnel branches two ways instead of one. Per-frame recorded
//! points accumulate in a private [`GestureState`] (owned alongside
//! [`AutomationLatches`] on the playback side, never the `Project`) rather
//! than being written into `PresetInstance::automation_lanes` as they
//! happen: punch-over's "this frame's touch overwrites what's ahead of it"
//! rule can't tell "not recorded yet" (an untouched pre-gesture point far in
//! the future) from "recorded earlier this same gesture, about to be
//! overwritten again" if both live in the same `Vec` — accumulating
//! privately and joining once, at gesture end, sidesteps that bug
//! entirely. A gesture closes on ~2 beats of touch inactivity
//! ([`GESTURE_INACTIVITY_BEATS`]), regardless of `armed` (so a mid-gesture
//! disarm still punches out instead of lingering forever), at which point
//! [`close_expired_gestures`] joins the recorded segment onto the
//! pre-gesture curve (old points before punch-in + the recorded segment +
//! old points after the last touch — the untouched tail is byte-identical
//! to the pre-gesture curve, which is what "the old curve resumes exactly"
//! means) and returns one `CommitRecordedGestureCommand` per finished
//! gesture for the caller to run through `EditingService` — the single
//! undo entry §5/§11.6 requires.

use ahash::AHashMap;

use manifold_core::effects::{AutomationPoint, PresetInstance, SegmentShape};
use manifold_core::project::Project;
use manifold_core::{Beats, EffectId, GraphTarget};
use manifold_editing::command::Command;
use manifold_editing::commands::automation::CommitRecordedGestureCommand;

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

/// Beats of inactivity after the last recorded touch before an in-progress
/// gesture is considered finished and committed (§5's "~2 beats of
/// inactivity ends the gesture").
const GESTURE_INACTIVITY_BEATS: f64 = 2.0;

/// One in-progress recording gesture (§5). Runtime-only, owned by
/// `PlaybackEngine` alongside [`AutomationLatches`] — never the `Project`,
/// never serialized. A gesture is a live in-flight performance action; it
/// isn't expected to survive a save/load (nothing persists it, same as a
/// latch).
pub struct GestureState {
    /// Where the eventual `CommitRecordedGestureCommand` writes.
    target: GraphTarget,
    /// The first touched frame's beat (the punch-in point).
    punch_in_beat: Beats,
    /// The most recent touched frame's beat (the eventual punch-out point).
    /// Updated every frame this gesture records a new point.
    last_touch_beat: Beats,
    /// The lane's point set immediately before this gesture started — the
    /// explicit reverse `CommitRecordedGestureCommand` carries (the
    /// `EditParamMappingCommand::new_with_reverse` precedent: captured at
    /// gesture START, before any recording mutated anything, not
    /// self-snapshotted after the fact).
    pre_gesture_points: Vec<AutomationPoint>,
    /// Whether a lane already existed for this param before the gesture
    /// started. `false` means this gesture is creating a brand new lane —
    /// undo must then remove the whole lane, mirroring
    /// `AddAutomationPointCommand`'s `created_lane` behavior.
    pre_gesture_existed: bool,
    /// Points recorded so far this gesture, sorted ascending by beat.
    recorded: Vec<AutomationPoint>,
}

/// Runtime-only in-flight recording gestures, keyed like [`AutomationLatches`].
/// Owned by `PlaybackEngine`.
pub type AutomationGestures = AHashMap<(EffectId, ParamId), GestureState>;

/// Sample every enabled automation lane at `current_beat` and write the
/// result onto each param's `base`. Call this BEFORE
/// `modulation::evaluate_modulation` each tick — automation is a hand, not a
/// modulator, so it must land before the base→value reset.
///
/// `armed` is the global Automation Arm (§5): while on, a touched param
/// records into its lane instead of latching an override. Returns
/// `(any_wrote, gesture_commits)` — `any_wrote` folds into the same
/// compositor-dirty path as `modulation_active` (never bumps the project
/// `DataVersion`); `gesture_commits` is one `CommitRecordedGestureCommand`
/// per gesture that finished this tick, for the caller to run through
/// `EditingService` (this function never touches undo itself).
pub fn evaluate_all_automation(
    project: &mut Project,
    current_beat: Beats,
    latches: &mut AutomationLatches,
    armed: bool,
    gestures: &mut AutomationGestures,
) -> (bool, Vec<Box<dyn Command>>) {
    let mut any_wrote = false;

    for fx in project.settings.master_effects.iter_mut() {
        let target = GraphTarget::Effect(fx.id.clone());
        if evaluate_instance_automation(fx, &target, current_beat, latches, armed, gestures) {
            any_wrote = true;
        }
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                let target = GraphTarget::Effect(fx.id.clone());
                if evaluate_instance_automation(fx, &target, current_beat, latches, armed, gestures) {
                    any_wrote = true;
                }
            }
        }
        let layer_id = layer.layer_id.clone();
        if let Some(gp) = layer.gen_params_mut() {
            let target = GraphTarget::Generator(layer_id);
            if evaluate_instance_automation(gp, &target, current_beat, latches, armed, gestures) {
                any_wrote = true;
            }
        }
    }

    // Gesture closure runs once, AFTER the walk above has had a chance to
    // update `last_touch_beat` for anything touched THIS frame — a gesture
    // becomes eligible for closure exactly when nothing touched it this
    // frame and `GESTURE_INACTIVITY_BEATS` have passed since the last frame
    // that did. Runs unconditionally (not gated on `armed`): a disarm or a
    // transport stop-then-resume must still punch a lingering gesture out
    // via the beat-gap rule, never leave it dangling.
    let commits = close_expired_gestures(gestures, current_beat);

    (any_wrote, commits)
}

/// Evaluate every automation lane on a single instance, plus (when armed)
/// any touched param that has no lane yet at all — the "arm creates a lane"
/// path (§5). Returns true if any lane wrote a value or recorded a point.
fn evaluate_instance_automation(
    fx: &mut PresetInstance,
    target: &GraphTarget,
    current_beat: Beats,
    latches: &mut AutomationLatches,
    armed: bool,
    gestures: &mut AutomationGestures,
) -> bool {
    if !fx.enabled {
        return false;
    }
    let has_lanes = fx.automation_lanes.as_ref().is_some_and(|v| !v.is_empty());
    let any_touched = fx.params.iter().any(|p| p.touched);
    // Nothing to do unless there's an existing lane to sample/latch/continue
    // recording, or (armed) a touch that might be starting a brand new
    // gesture — see docs/AUTOMATION_LANES_DESIGN.md §5 "arm creates a lane".
    if !(has_lanes || (armed && any_touched)) {
        return false;
    }
    let fx_id = fx.id.clone();

    // Pass 1 (immutable): classify every enabled lane's param — continue an
    // open gesture (handled below via the touched flag, not here), start a
    // new one (armed touch), latch (unarmed touch), stay latched, or sample.
    let mut to_latch: Vec<ParamId> = Vec::new();
    let mut to_write: Vec<(ParamId, f32)> = Vec::new();
    let mut to_record: Vec<(ParamId, f32)> = Vec::new();
    let mut handled_ids: Vec<ParamId> = Vec::new();

    if has_lanes {
        let lanes = fx.automation_lanes.as_ref().unwrap();
        for lane in lanes.iter().filter(|l| l.enabled) {
            // Resolve directly against the id-keyed manifest — range +
            // integral-ness ride each `Param.spec`, no registry consultation.
            let Some(p) = fx.params.get(lane.param_id.as_ref()) else {
                continue;
            };
            handled_ids.push(lane.param_id.clone());
            if p.touched {
                if armed {
                    to_record.push((lane.param_id.clone(), p.base));
                } else {
                    to_latch.push(lane.param_id.clone());
                }
                continue;
            }
            let key = (fx_id.clone(), lane.param_id.clone());
            if gestures.contains_key(&key) {
                // Waiting within the gesture's inactivity grace window —
                // nothing new to record this frame, and NOT a sample
                // (recording owns this param until the gesture closes).
                continue;
            }
            if latches.contains_key(&key) {
                continue; // still overridden
            }
            // BUG-039: a periodic param (e.g. a rotation the performer
            // authored a multi-turn ramp on, points climbing past `max`)
            // wraps back into range on read instead of clamping and
            // plateauing at the rail — the stored `points` are untouched,
            // only this per-frame interpretation changes.
            let raw = lane.value_at(current_beat);
            let value = manifold_core::params::constrain_to_range(
                raw,
                p.spec.min,
                p.spec.max,
                p.spec.wraps,
            );
            to_write.push((lane.param_id.clone(), value));
        }
    }

    // Pass over touched params NOT covered by an existing enabled lane
    // above (armed only): this is how a lane is BORN from performance (§5).
    // Includes a currently-disabled lane's param, which `param_id_for_idx`
    // + the gesture-start lookup below treat the same as "not currently
    // automated" from the touch's perspective.
    if armed {
        for p in fx.params.iter() {
            if !p.touched {
                continue;
            }
            let param_id = ParamId::from(p.id().to_string());
            if handled_ids.contains(&param_id) {
                continue;
            }
            to_record.push((param_id, p.base));
        }
    }

    // Pass 2 (mutable): clear the touched flag + latch for params that were
    // just touched, then write sampled values via the non-touching
    // automation write path (using `set_base_param` here would set
    // `touched` on our own write and self-latch the very next frame — see
    // `docs/AUTOMATION_LANES_DESIGN.md` §4), then feed recording gestures.
    for param_id in to_latch {
        if let Some(p) = fx.params.get_mut(param_id.as_ref()) {
            p.touched = false;
        }
        latches.insert((fx_id.clone(), param_id), ());
    }
    let mut any_wrote = false;
    for (param_id, value) in to_write {
        fx.set_base_param_from_automation(param_id.as_ref(), value);
        any_wrote = true;
    }
    for (param_id, applied_base) in to_record {
        if let Some(p) = fx.params.get_mut(param_id.as_ref()) {
            p.touched = false;
        }
        // Recording supersedes any stale override latch from before arm was
        // engaged — once the hand is recording, the param isn't "overridden"
        // in the Back-to-Arrangement sense, it's authoring new automation.
        latches.remove(&(fx_id.clone(), param_id.clone()));

        let key = (fx_id.clone(), param_id.clone());
        let gesture = gestures.entry(key).or_insert_with(|| {
            let (pre_points, existed) = fx
                .automation_lanes
                .as_ref()
                .and_then(|lanes| lanes.iter().find(|l| l.param_id == param_id))
                .map(|lane| (lane.points.clone(), true))
                .unwrap_or_else(|| (Vec::new(), false));
            GestureState {
                target: target.clone(),
                punch_in_beat: current_beat,
                last_touch_beat: current_beat,
                pre_gesture_points: pre_points,
                pre_gesture_existed: existed,
                recorded: Vec::new(),
            }
        });
        // Punch-over: drop any already-recorded points at/after this beat —
        // a loop or backward scrub re-passing the same range overwrites it —
        // then append the new sample.
        gesture.recorded.retain(|p| p.beat.0 < current_beat.0);
        gesture.recorded.push(AutomationPoint {
            beat: current_beat,
            value: applied_base,
            shape: SegmentShape::Linear,
        });
        gesture.last_touch_beat = current_beat;
        any_wrote = true;
    }
    any_wrote
}

/// Close every gesture that has gone `GESTURE_INACTIVITY_BEATS` without a
/// new touch: join the recorded segment onto the pre-gesture curve (old
/// points before punch-in + the recorded segment + old points after the
/// last touch, so the untouched tail is byte-identical to before — "the old
/// curve resumes exactly", §5) and return one `CommitRecordedGestureCommand`
/// per closed gesture for the caller to run through `EditingService`.
fn close_expired_gestures(
    gestures: &mut AutomationGestures,
    current_beat: Beats,
) -> Vec<Box<dyn Command>> {
    let expired: Vec<(EffectId, ParamId)> = gestures
        .iter()
        .filter(|(_, g)| current_beat.0 - g.last_touch_beat.0 >= GESTURE_INACTIVITY_BEATS)
        .map(|(key, _)| key.clone())
        .collect();

    let mut commits: Vec<Box<dyn Command>> = Vec::with_capacity(expired.len());
    for key in expired {
        let Some(gesture) = gestures.remove(&key) else {
            continue;
        };
        let (_, param_id) = key;

        let mut new_points: Vec<AutomationPoint> = gesture
            .pre_gesture_points
            .iter()
            .filter(|p| p.beat.0 < gesture.punch_in_beat.0)
            .copied()
            .collect();
        new_points.extend(gesture.recorded.iter().copied());
        new_points.extend(
            gesture
                .pre_gesture_points
                .iter()
                .filter(|p| p.beat.0 > gesture.last_touch_beat.0)
                .copied(),
        );

        let old_points = gesture.pre_gesture_existed.then_some(gesture.pre_gesture_points);
        commits.push(Box::new(CommitRecordedGestureCommand::new(
            gesture.target,
            param_id,
            new_points,
            old_points,
        )) as Box<dyn Command>);
    }
    commits
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

    /// Calls `evaluate_all_automation` with arm off, discarding gesture
    /// commits (there can't be any with arm off) — the P1-era call shape,
    /// kept as a thin wrapper so the pre-existing P1 tests below didn't need
    /// to change.
    fn eval_unarmed(
        project: &mut Project,
        beat: Beats,
        latches: &mut AutomationLatches,
    ) -> bool {
        let mut gestures = AutomationGestures::default();
        let (wrote, commits) = evaluate_all_automation(project, beat, latches, false, &mut gestures);
        assert!(commits.is_empty(), "arm off must never produce a gesture commit");
        wrote
    }

    #[test]
    fn samples_and_writes_base_at_current_beat() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        let wrote = eval_unarmed(&mut project, Beats(2.0), &mut latches);

        assert!(wrote, "an enabled lane with no override reports a write");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        // Halfway between beat 0 (0.2) and beat 4 (0.8) -> 0.5.
        assert!(
            (fx.params.get("amount").unwrap().base - 0.5).abs() < 1e-6,
            "base sampled at the midpoint, got {}",
            fx.params.get("amount").unwrap().base
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

        assert!(eval_unarmed(&mut project, Beats(4.0), &mut latches));
        crate::modulation::reset_all_effectives(&mut project);

        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.params.get("amount").unwrap().value - 0.8).abs() < 1e-6,
            "reset_all_effectives must copy the automation-sampled base into \
             value, proving automation ran first; got {}",
            fx.params.get("amount").unwrap().value
        );
    }

    #[test]
    fn touch_latches_and_skips_the_write_that_frame() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        // Simulate a live hand touching the param this frame (a UI slider,
        // Ableton macro, or OSC write all funnel through set_base_param).
        fx.set_base_param("amount", 0.99);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        let wrote = eval_unarmed(&mut project, Beats(2.0), &mut latches);

        assert!(!wrote, "a freshly-touched param is not sampled this frame");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.params.get("amount").unwrap().base - 0.99).abs() < 1e-6,
            "the hand's write wins, untouched by automation"
        );
        assert!(
            !fx.params.get("amount").unwrap().touched,
            "the evaluator clears touched once it has latched"
        );
        assert_eq!(latches.len(), 1, "the touch latches the param as overridden");
    }

    #[test]
    fn still_latched_param_stays_skipped_on_later_frames() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.set_base_param("amount", 0.99);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // Frame 1: touch → latch, no write.
        assert!(!eval_unarmed(&mut project, Beats(2.0), &mut latches));
        // Frame 2: no new touch, but still latched — still no write, base
        // stays at the hand's value, not automation's.
        let wrote = eval_unarmed(&mut project, Beats(2.0), &mut latches);
        assert!(!wrote, "a still-latched param stays overridden");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!((fx.params.get("amount").unwrap().base - 0.99).abs() < 1e-6);
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
        assert!(eval_unarmed(&mut project, Beats(1.0), &mut latches));
        assert!(latches.is_empty(), "a sampled write must not self-latch");

        // Frame 2: still samples normally — proves frame 1's write didn't
        // leave the param looking "touched" to frame 2's evaluator.
        let wrote = eval_unarmed(&mut project, Beats(3.0), &mut latches);
        assert!(wrote, "automation keeps sampling across frames uninterrupted");
        assert!(latches.is_empty());
    }

    #[test]
    fn back_to_arrangement_resumes_a_latched_lane() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.set_base_param("amount", 0.99);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // Touch latches the lane.
        assert!(!eval_unarmed(&mut project, Beats(2.0), &mut latches));
        assert_eq!(latches.len(), 1);

        // Back to Arrangement: `PlaybackEngine::automation_back_to_arrangement`
        // is exactly `self.automation_latches.clear()` — exercised directly
        // here at the map level (the engine wrapper is a one-line delegation).
        latches.clear();

        let wrote = eval_unarmed(&mut project, Beats(2.0), &mut latches);
        assert!(wrote, "clearing the latch resumes the lane's writes");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            (fx.params.get("amount").unwrap().base - 0.5).abs() < 1e-6,
            "lane resumes sampling at the current beat, got {}",
            fx.params.get("amount").unwrap().base
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

        assert!(!eval_unarmed(&mut project, Beats(2.0), &mut latches));
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert_eq!(fx.params.get("amount").unwrap().base, 0.0, "untouched, still the default");
    }

    #[test]
    fn disabled_effect_is_skipped_entirely() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.enabled = false;
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        assert!(!eval_unarmed(&mut project, Beats(2.0), &mut latches));
    }

    #[test]
    fn empty_lanes_list_is_a_cheap_noop() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(Vec::new());
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        assert!(!eval_unarmed(&mut project, Beats(2.0), &mut latches));
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
            eval_unarmed(&mut project, Beats(beat), &mut latches);
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

        let wrote = eval_unarmed(&mut project, Beats(4.0), &mut latches);
        assert!(wrote);
        let gp = project.timeline.layers[0].gen_params().unwrap();
        assert!((gp.params.get("amount").unwrap().base - 0.8).abs() < 1e-6);
    }

    // ─────────────────────────────────────────────────────────────
    // P3: recording (arm, gesture capture, punch boundaries, single-undo).
    // ─────────────────────────────────────────────────────────────

    #[test]
    fn arm_off_touch_still_latches_p1_behavior_unchanged() {
        // Arm off is the default; a touch on an automated param must still
        // latch exactly as P1 shipped it, not silently start recording.
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.set_base_param("amount", 0.42);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        let (wrote, commits) =
            evaluate_all_automation(&mut project, Beats(2.0), &mut latches, false, &mut gestures);

        assert!(!wrote);
        assert!(commits.is_empty());
        assert_eq!(latches.len(), 1, "unarmed touch still latches");
        assert!(gestures.is_empty(), "no gesture starts with arm off");
    }

    #[test]
    fn armed_touch_on_existing_lane_starts_a_gesture_not_a_latch() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]);
        fx.set_base_param("amount", 0.42);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        let (wrote, commits) =
            evaluate_all_automation(&mut project, Beats(1.0), &mut latches, true, &mut gestures);

        assert!(wrote, "a recorded touch is activity worth a UI refresh");
        assert!(commits.is_empty(), "the gesture hasn't closed yet");
        assert!(latches.is_empty(), "armed recording does not latch");
        assert_eq!(gestures.len(), 1, "the touch opened one gesture");
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            !fx.params.get("amount").unwrap().touched,
            "recording consumes the touch flag same as latching does"
        );
    }

    #[test]
    fn armed_touch_with_no_lane_creates_one_on_commit() {
        // "Arm creates a lane" (§5): no lane exists yet for `amount`, but a
        // touch while armed still opens a gesture and, once it closes,
        // yields a commit whose `old_points` is `None` (lane creation).
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.set_base_param("amount", 0.55); // no automation_lanes at all yet
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        let (wrote, commits) =
            evaluate_all_automation(&mut project, Beats(1.0), &mut latches, true, &mut gestures);
        assert!(wrote);
        assert!(commits.is_empty());
        assert_eq!(gestures.len(), 1, "arm creates a lane by opening a gesture");

        // Let the gesture go quiet for >= 2 beats to force closure.
        let (_, commits) =
            evaluate_all_automation(&mut project, Beats(3.5), &mut latches, true, &mut gestures);
        assert_eq!(commits.len(), 1, "the gesture closes after ~2 beats of inactivity");
        assert!(gestures.is_empty());

        let mut cmd = commits.into_iter().next().unwrap();
        let fx_id = project.timeline.layers[0].effects.as_ref().unwrap()[0].id.clone();
        cmd.execute(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        let lane = fx
            .automation_lanes
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.param_id.as_ref() == "amount")
            .unwrap();
        assert_eq!(lane.points.len(), 1, "one recorded point at the punch-in beat");
        assert!((lane.points[0].value - 0.55).abs() < 1e-6);

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        assert!(
            fx.automation_lanes.as_ref().is_none_or(|v| v.is_empty()),
            "undo of a lane-creating gesture removes the whole lane"
        );
    }

    #[test]
    fn evaluator_self_write_does_not_record() {
        // The recording-side footgun analogous to the latch-side one above:
        // automation's own sampled write (via set_base_param_from_automation,
        // which never sets `touched`) must not be mistaken for a live touch
        // and start a gesture, even while armed.
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        let (wrote, commits) =
            evaluate_all_automation(&mut project, Beats(1.0), &mut latches, true, &mut gestures);
        assert!(wrote, "still samples normally — no live touch, so no gesture");
        assert!(commits.is_empty());
        assert!(
            gestures.is_empty(),
            "the evaluator's own sampled write must not be read back as a touch"
        );
    }

    #[test]
    fn gesture_commits_one_undo_entry_with_pre_gesture_reverse() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.automation_lanes = Some(vec![amount_lane()]); // points at beat 0 (0.2), beat 4 (0.8)
        let mut project = project_with(layer);
        let fx_id = project.timeline.layers[0].effects.as_ref().unwrap()[0].id.clone();
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        // Touch at beat 1, recording a new value over the existing curve.
        project
            .timeline
            .layers[0]
            .effects
            .as_mut()
            .unwrap()[0]
            .set_base_param("amount", 0.6);
        evaluate_all_automation(&mut project, Beats(1.0), &mut latches, true, &mut gestures);

        // Close the gesture (>= 2 beats quiet).
        let (_, commits) =
            evaluate_all_automation(&mut project, Beats(3.5), &mut latches, true, &mut gestures);
        assert_eq!(commits.len(), 1, "exactly one undo entry per gesture");

        let mut cmd = commits.into_iter().next().unwrap();
        cmd.execute(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        let lane = fx
            .automation_lanes
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.param_id.as_ref() == "amount")
            .unwrap();
        // Pre-gesture point at beat 0 survives (before punch-in); the
        // recorded point at beat 1 (0.6) replaces the old curve there; the
        // pre-gesture point at beat 4 survives (after the last touch) —
        // "the old curve resumes exactly" past the gesture.
        assert!(lane.points.iter().any(|p| p.beat.0 == 0.0 && (p.value - 0.2).abs() < 1e-6));
        assert!(lane.points.iter().any(|p| p.beat.0 == 1.0 && (p.value - 0.6).abs() < 1e-6));
        assert!(lane.points.iter().any(|p| p.beat.0 == 4.0 && (p.value - 0.8).abs() < 1e-6));

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        let lane = fx
            .automation_lanes
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.param_id.as_ref() == "amount")
            .unwrap();
        assert_eq!(lane.points.len(), 2, "undo restores the exact pre-gesture point set");
        assert!((lane.points[0].value - 0.2).abs() < 1e-6);
        assert!((lane.points[1].value - 0.8).abs() < 1e-6);
    }

    #[test]
    fn inactivity_under_two_beats_keeps_the_gesture_open() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.set_base_param("amount", 0.3);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        evaluate_all_automation(&mut project, Beats(0.0), &mut latches, true, &mut gestures);
        assert_eq!(gestures.len(), 1);

        // Only 1.5 beats of silence — must NOT close yet.
        let (_, commits) =
            evaluate_all_automation(&mut project, Beats(1.5), &mut latches, true, &mut gestures);
        assert!(commits.is_empty(), "under the ~2 beat threshold, the gesture stays open");
        assert_eq!(gestures.len(), 1);
    }

    // ── BUG-039: automation ramp wrap ───────────────────────────────────

    /// A ramp with a point authored past the param's own `max` (a
    /// multi-turn rotation ramp, e.g. drawn from 0 to 720 on a 0..360
    /// param) reads back wrapped into range instead of plateauing at the
    /// rail once the interpolated value crosses `max`.
    #[test]
    fn ramp_past_max_wraps_when_param_is_periodic() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.params.get_mut("amount").unwrap().spec.min = 0.0;
        fx.params.get_mut("amount").unwrap().spec.max = 360.0;
        fx.params.get_mut("amount").unwrap().spec.wraps = true;
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![
                AutomationPoint { beat: Beats(0.0), value: 0.0, shape: SegmentShape::Linear },
                // A continuous multi-turn ramp: 720 = two full rotations.
                AutomationPoint { beat: Beats(4.0), value: 720.0, shape: SegmentShape::Linear },
            ],
        }]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // 3/4 of the way through -> raw interpolated 540, wrapped into
        // [0,360): 540 - 360 = 180.
        eval_unarmed(&mut project, Beats(3.0), &mut latches);
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        let value = fx.params.get("amount").unwrap().base;
        assert!(
            (value - 180.0).abs() < 1e-4,
            "a ramp past max must wrap continuously, got {value}"
        );
    }

    /// The same overshoot on a non-periodic param (`wraps` stays false, the
    /// pre-fix behavior) still clamps at the rail and plateaus — proving the
    /// wrap is opt-in, not a silent behavior change for every ramp.
    #[test]
    fn ramp_past_max_clamps_when_param_is_not_periodic() {
        let mut layer = layer_with_one_effect();
        let fx = &mut layer.effects.as_mut().unwrap()[0];
        fx.params.get_mut("amount").unwrap().spec.min = 0.0;
        fx.params.get_mut("amount").unwrap().spec.max = 360.0;
        // wraps stays false (default).
        fx.automation_lanes = Some(vec![AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: vec![
                AutomationPoint { beat: Beats(0.0), value: 0.0, shape: SegmentShape::Linear },
                AutomationPoint { beat: Beats(4.0), value: 720.0, shape: SegmentShape::Linear },
            ],
        }]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        // 3/4 through -> raw interpolated 540, clamped to max 360.
        eval_unarmed(&mut project, Beats(3.0), &mut latches);
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        let value = fx.params.get("amount").unwrap().base;
        assert!(
            (value - 360.0).abs() < 1e-4,
            "a non-wrapping param must clamp (plateau) at max, got {value}"
        );
    }
}
