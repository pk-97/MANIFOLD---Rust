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
//! The playback walk follows `modulation::evaluate_all_audio_mods`: master
//! effects + layer effects + generator params, skip disabled instances,
//! resolve via `resolve_param_in`, two-pass resolve-then-write. Its recording
//! path uses the same small per-instance scratch vectors as that existing
//! evaluator. Stopped/paused inspection uses a separate direct walk so the
//! steady-state preview path remains allocation-free.
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
//! ## Recording (section 5)
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
//! undo entry section 5/section 11.6 requires.
//!
//! A take lasts while the hand is on the control (Ableton's arm): an input
//! with a release (a mouse held on a slider, [`AutomationWrite::holds`])
//! keeps writing its held value while still and ends on release; inputs with
//! no release (MIDI CC, OSC) end after [`GESTURE_INACTIVITY_BEATS`]. A loop
//! wrap or backward seek starts a new pass that overwrites only the beats it
//! covers. Each pass is thinned as its points arrive (`PassThinner`, constant
//! work per point) and joins the curve through buffers the take owns, so a
//! loop wrap or release costs one linear copy and no allocation once the
//! buffers have grown to the take's size.

use ahash::AHashMap;

use manifold_core::effects::{AutomationLane, AutomationPoint, PresetInstance, SegmentShape};
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
/// gesture is considered finished and committed (section 5's "~2 beats of
/// inactivity ends the gesture").
const GESTURE_INACTIVITY_BEATS: f64 = 2.0;

/// Largest value error thinning may introduce, as a fraction of the param's
/// slider range.
const THINNING_TOLERANCE: f64 = 0.005;

/// Params a pointer is holding down right now (mouse down on a slider), by
/// graph target. Runtime-only, owned by `PlaybackEngine`.
pub type AutomationHolds = Vec<(GraphTarget, ParamId)>;

/// One tick's recording inputs.
#[derive(Debug, Clone, Copy)]
pub struct AutomationWrite<'a> {
    /// The global Automation Arm (section 5).
    pub armed: bool,
    /// Params under a held pointer: they keep writing while still, and their
    /// gesture ends on release rather than on inactivity.
    pub holds: &'a [(GraphTarget, ParamId)],
}

impl AutomationWrite<'static> {
    /// No held pointer: the shape of an input with no release.
    pub const fn unheld(armed: bool) -> Self {
        AutomationWrite { armed, holds: &[] }
    }
}

impl AutomationWrite<'_> {
    fn held(&self, target: &GraphTarget, param_id: &str) -> bool {
        self.holds
            .iter()
            .any(|(t, p)| t == target && p.as_ref() == param_id)
    }
}

/// One in-progress recording gesture (section 5). Runtime-only, owned by
/// `PlaybackEngine` alongside [`AutomationLatches`] — never the `Project`,
/// never serialized. A gesture is a live in-flight performance action; it
/// isn't expected to survive a save/load (nothing persists it, same as a
/// latch).
pub struct GestureState {
    /// Where the eventual `CommitRecordedGestureCommand` writes.
    target: GraphTarget,
    /// The most recent touched frame's sampled beat (the eventual punch-out
    /// point): the take holds its last value through the instant that frame
    /// was read, so replay shows it there. Updated every frame this gesture
    /// records a new point.
    last_touch_beat: Beats,
    /// The lane's point set immediately before this gesture started — the
    /// explicit reverse `CommitRecordedGestureCommand` carries (the
    /// `EditParamMappingCommand::new_with_reverse` precedent: captured at
    /// gesture START, before any recording mutated anything, not
    /// self-snapshotted after the fact).
    /// Held as a lane so the punch-in can sample it in place.
    pre_gesture: AutomationLane,
    /// Whether a lane already existed for this param before the gesture
    /// started. `false` means this gesture is creating a brand new lane —
    /// undo must then remove the whole lane, mirroring
    /// `AddAutomationPointCommand`'s `created_lane` behavior.
    pre_gesture_existed: bool,
    /// The pre-gesture curve with this gesture's finished passes punched in.
    /// Meaningful only once `layered_live` is set, by the first loop wrap or
    /// backward seek.
    layered: AutomationLane,
    layered_live: bool,
    /// The buffer the next punch-in writes before it swaps with `layered`.
    scratch: Vec<AutomationPoint>,
    /// The current pass's points, already thinned, sorted ascending by beat.
    recorded: Vec<AutomationPoint>,
    thinner: PassThinner,
    /// The pointer holding this param let go: the gesture ends on the next
    /// tick.
    released: bool,
    /// Whole-number param: thinning keeps only value changes.
    stepped: bool,
}

/// Points a pass holds before its buffers first grow. A pass that outgrows
/// them doubles them once and keeps the room for every later pass.
const PASS_POINT_CAPACITY: usize = 256;

impl GestureState {
    /// Open a take. Every buffer the take reuses is sized here, at the touch
    /// that starts it, not on a later loop downbeat.
    fn new(
        target: GraphTarget,
        param_id: ParamId,
        pre_points: Option<Vec<AutomationPoint>>,
        range: f32,
        stepped: bool,
        current_beat: Beats,
    ) -> Self {
        let pre_gesture_existed = pre_points.is_some();
        let pre_points = pre_points.unwrap_or_default();
        let curve_capacity = pre_points.len() + PASS_POINT_CAPACITY + 2;
        let lane = |points| AutomationLane {
            param_id: param_id.clone(),
            enabled: true,
            points,
        };
        GestureState {
            target,
            last_touch_beat: current_beat,
            pre_gesture: lane(pre_points),
            pre_gesture_existed,
            layered: lane(Vec::with_capacity(curve_capacity)),
            layered_live: false,
            scratch: Vec::with_capacity(curve_capacity),
            recorded: Vec::with_capacity(PASS_POINT_CAPACITY),
            thinner: PassThinner::new(range, stepped),
            released: false,
            stepped,
        }
    }

    /// Append one frame's point to the current pass, thinned as it lands. A
    /// point behind the pass (a loop wrap or backward seek) finishes the pass
    /// first, so the new pass overwrites only the beats it covers. A held-still
    /// control slides one end point forward and never grows the pass.
    fn record(&mut self, point: AutomationPoint, sampled_beat: Beats) {
        if self
            .recorded
            .last()
            .is_some_and(|last| point.beat < last.beat)
        {
            self.finish_pass();
        }
        self.thinner.push(&mut self.recorded, point);
        self.last_touch_beat = sampled_beat;
    }

    /// Punch the current pass into `layered`: one linear copy into `scratch`,
    /// then the two buffers swap.
    fn finish_pass(&mut self) {
        let Some(&last) = self.recorded.last() else {
            return;
        };
        // Points sit on their frames' exact beats; the old curve resumes only
        // after the last touched frame's sampled beat, where replay reads it.
        if self.last_touch_beat > last.beat {
            let end = AutomationPoint {
                beat: self.last_touch_beat,
                value: last.value,
                shape: SegmentShape::Hold,
            };
            self.thinner.push(&mut self.recorded, end);
        }
        let base = if self.layered_live {
            &self.layered
        } else {
            &self.pre_gesture
        };
        punch_recorded_points_into(base, &self.recorded, &mut self.scratch);
        std::mem::swap(&mut self.layered.points, &mut self.scratch);
        self.layered_live = true;
        self.recorded.clear();
        self.thinner.reset();
    }

    /// Finish the take as its one undo entry.
    fn into_command(mut self, param_id: ParamId) -> Box<dyn Command> {
        self.finish_pass();
        let new_points = if self.layered_live {
            self.layered.points
        } else {
            self.pre_gesture.points.clone()
        };
        let old_points = self.pre_gesture_existed.then_some(self.pre_gesture.points);
        Box::new(CommitRecordedGestureCommand::new(
            self.target,
            param_id,
            new_points,
            old_points,
        ))
    }

    /// Whether this gesture's take ends this tick.
    fn ends(&self, param_id: &str, current_beat: Beats, write: &AutomationWrite<'_>) -> bool {
        if self.released {
            return true;
        }
        !write.held(&self.target, param_id)
            && current_beat.0 - self.last_touch_beat.0 >= GESTURE_INACTIVITY_BEATS
    }
}

/// A held pointer let go of `param_id` on `target`: its open take ends on the
/// next tick.
pub fn release_hold(gestures: &mut AutomationGestures, target: &GraphTarget, param_id: &str) {
    for ((_, key_param_id), gesture) in gestures.iter_mut() {
        if gesture.target == *target && key_param_id.as_ref() == param_id {
            gesture.released = true;
        }
    }
}

/// Thins one recorded pass as its points arrive, with constant work per point
/// and no buffer of its own, so a long take never pays a burst at its end.
///
/// The pass keeps its first point, its newest point, and every point the
/// line between kept neighbours could not reproduce. Continuous params use
/// the slope-cone test (an opening-window line simplification): from the
/// last kept point (the anchor), each dropped point allows the slopes whose
/// line passes within [`THINNING_TOLERANCE`] of `range` of it, and the newest
/// point may replace the one before it only while its own slope from the
/// anchor stays inside every dropped point's allowance. So no dropped point
/// strays more than the tolerance from the line replay draws over it.
/// Stepped params keep only the points where the value changes, plus the
/// newest point.
#[derive(Debug, Clone, Copy)]
struct PassThinner {
    tolerance: f64,
    stepped: bool,
    /// Slopes from the anchor that keep every point dropped since it in
    /// tolerance.
    slope_lo: f64,
    slope_hi: f64,
    /// The most recently dropped point: a valid end for the anchor's segment,
    /// kept so two frames on one beat can fall back to it.
    dropped: Option<AutomationPoint>,
    /// Stepped: the newest point repeats its predecessor's value, so it is a
    /// run's tail and the next point replaces it.
    tail: bool,
}

impl PassThinner {
    fn new(range: f32, stepped: bool) -> Self {
        PassThinner {
            tolerance: THINNING_TOLERANCE * if range > 0.0 { f64::from(range) } else { 1.0 },
            stepped,
            slope_lo: f64::NEG_INFINITY,
            slope_hi: f64::INFINITY,
            dropped: None,
            tail: false,
        }
    }

    /// Forget the pass: the next point starts a new one.
    fn reset(&mut self) {
        self.open_segment();
        self.tail = false;
    }

    /// A new anchor: nothing dropped since it.
    fn open_segment(&mut self) {
        self.slope_lo = f64::NEG_INFINITY;
        self.slope_hi = f64::INFINITY;
        self.dropped = None;
    }

    /// Add the pass's next point to `points` (beat at or after the newest).
    fn push(&mut self, points: &mut Vec<AutomationPoint>, point: AutomationPoint) {
        if self.stepped {
            self.push_stepped(points, point);
        } else {
            self.push_continuous(points, point);
        }
    }

    fn push_stepped(&mut self, points: &mut Vec<AutomationPoint>, point: AutomationPoint) {
        // A second frame on one beat supersedes the first. What remains last
        // is always a kept change point, which is also the dropped frame's
        // predecessor's value.
        if points.last().is_some_and(|last| last.beat == point.beat) {
            points.pop();
            self.tail = false;
        }
        let Some(&last) = points.last() else {
            points.push(point);
            return;
        };
        if self.tail {
            *points.last_mut().unwrap() = point;
        } else {
            points.push(point);
        }
        self.tail = point.value == last.value;
    }

    fn push_continuous(&mut self, points: &mut Vec<AutomationPoint>, point: AutomationPoint) {
        let n = points.len();
        if n < 2 {
            match points.last_mut() {
                Some(only) if only.beat == point.beat => *only = point,
                _ => points.push(point),
            }
            self.open_segment();
            return;
        }
        let (anchor, last) = (points[n - 2], points[n - 1]);
        if point.beat == last.beat {
            // A second frame on one beat supersedes the first, if the points
            // already dropped still fit the line to it; otherwise the last
            // dropped point becomes the anchor and the new frame follows it.
            if !self.fits(anchor, point, self.slope_lo, self.slope_hi)
                && let Some(dropped) = self.dropped
            {
                points[n - 1] = dropped;
                points.push(point);
                self.open_segment();
            } else {
                points[n - 1] = point;
            }
            return;
        }
        let (lo, hi) = self.allowance(anchor, last);
        let (lo, hi) = (self.slope_lo.max(lo), self.slope_hi.min(hi));
        if self.fits(anchor, point, lo, hi) {
            points[n - 1] = point;
            self.slope_lo = lo;
            self.slope_hi = hi;
            self.dropped = Some(last);
        } else {
            points.push(point);
            self.open_segment();
        }
    }

    /// Slopes from `anchor` whose line passes within tolerance of `point`.
    fn allowance(&self, anchor: AutomationPoint, point: AutomationPoint) -> (f64, f64) {
        let run = point.beat.0 - anchor.beat.0;
        let rise = f64::from(point.value) - f64::from(anchor.value);
        ((rise - self.tolerance) / run, (rise + self.tolerance) / run)
    }

    /// Whether the line from `anchor` to `point` has a slope in `lo..=hi`.
    fn fits(&self, anchor: AutomationPoint, point: AutomationPoint, lo: f64, hi: f64) -> bool {
        let run = point.beat.0 - anchor.beat.0;
        if run <= 0.0 {
            return false;
        }
        let slope = (f64::from(point.value) - f64::from(anchor.value)) / run;
        lo <= slope && slope <= hi
    }
}

/// Runtime-only in-flight recording gestures, keyed like [`AutomationLatches`].
/// Owned by `PlaybackEngine`.
pub type AutomationGestures = AHashMap<(EffectId, ParamId), GestureState>;

/// Runtime-only automation curve being previewed by an editor gesture.
///
/// Preview points live outside the authoritative project lane so the editor
/// can render a draft without making an undoable project mutation. The base
/// value is captured before the first preview write and restored when the
/// preview is cleared, unless a real hand has taken ownership in the
/// meantime.
#[derive(Debug)]
pub struct AutomationLanePreview {
    pub lane: AutomationLane,
    pub original_base: f32,
}

/// Runtime-only previews keyed by their graph target and parameter id.
pub type AutomationLanePreviews = AHashMap<(GraphTarget, ParamId), AutomationLanePreview>;

/// Controls whether automation sampling may mutate recording state.
///
/// Stopped and paused ticks still need the arrangement curve for inspection,
/// but moving the playhead must not record a touch or close an in-flight
/// gesture. `Playback` preserves the normal armed-recording behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSamplingMode {
    Playback,
    Inspection,
    /// Arrangement-only export: never record, latch, or consume live touches.
    Export,
}

/// Sample every enabled automation lane at `current_beat` and write the
/// result onto each param's `base`. Call this BEFORE
/// `modulation::evaluate_modulation` each tick — automation is a hand, not a
/// modulator, so it must land before the base→value reset.
///
/// `armed` is the global Automation Arm (section 5): while on, a touched param
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
    evaluate_all_automation_with_mode(
        project,
        current_beat,
        latches,
        armed,
        gestures,
        AutomationSamplingMode::Playback,
    )
}

/// Evaluate automation with an explicit transport sampling mode and no held
/// pointer.
pub fn evaluate_all_automation_with_mode(
    project: &mut Project,
    current_beat: Beats,
    latches: &mut AutomationLatches,
    armed: bool,
    gestures: &mut AutomationGestures,
    mode: AutomationSamplingMode,
) -> (bool, Vec<Box<dyn Command>>) {
    evaluate_all_automation_at(
        project,
        current_beat,
        current_beat,
        latches,
        AutomationWrite::unheld(armed),
        gestures,
        mode,
    )
}

/// `evaluate_all_automation_with_mode` with the transport's two beats
/// (FRAME_EXACT_TRANSPORT_DESIGN D14): lanes are sampled at `current_beat`,
/// a hair past the frame, and touches are recorded at `frame_beat`, the
/// frame's exact beat, so a recorded step replays on the frame it was played.
pub fn evaluate_all_automation_at(
    project: &mut Project,
    current_beat: Beats,
    frame_beat: Beats,
    latches: &mut AutomationLatches,
    write: AutomationWrite<'_>,
    gestures: &mut AutomationGestures,
    mode: AutomationSamplingMode,
) -> (bool, Vec<Box<dyn Command>>) {
    if mode != AutomationSamplingMode::Playback {
        return evaluate_all_automation_inspection(
            project,
            current_beat,
            latches,
            gestures,
            mode == AutomationSamplingMode::Export,
        );
    }

    let mut any_wrote = false;
    let beats = (current_beat, frame_beat);

    for fx in project.settings.master_effects.iter_mut() {
        let target = GraphTarget::Effect(fx.id.clone());
        if evaluate_instance_automation(fx, &target, beats, latches, &write, gestures) {
            any_wrote = true;
        }
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                let target = GraphTarget::Effect(fx.id.clone());
                if evaluate_instance_automation(fx, &target, beats, latches, &write, gestures) {
                    any_wrote = true;
                }
            }
        }
        let layer_id = layer.layer_id.clone();
        if let Some(gp) = layer.gen_params_mut() {
            let target = GraphTarget::Generator(layer_id);
            if evaluate_instance_automation(gp, &target, beats, latches, &write, gestures) {
                any_wrote = true;
            }
        }
    }

    // Gesture closure runs once, AFTER the walk above has had a chance to
    // update `last_touch_beat` for anything touched THIS frame — a gesture
    // becomes eligible for closure exactly when nothing touched it this
    // frame and `GESTURE_INACTIVITY_BEATS` have passed since the last frame
    // that did, or its pointer let go. Runs unconditionally (not gated on
    // `armed`): a disarm or a transport stop-then-resume must still punch a
    // lingering gesture out, never leave it dangling. Inspection mode is the
    // deliberate exception: stopped/paused seeks must not close gestures.
    let commits = close_expired_gestures(gestures, current_beat, &write);

    (any_wrote, commits)
}

/// Evaluate runtime-only automation previews after arrangement automation and
/// before modulation. Preview writes use the same range constraint and base
/// writer as arrangement automation, but never set `touched` or create a
/// recording gesture. A live touch, latch, or recording gesture retains the
/// existing manual/recording semantics and suppresses the preview for that
/// parameter.
pub fn evaluate_automation_previews(
    project: &mut Project,
    current_beat: Beats,
    previews: &mut AutomationLanePreviews,
    latches: &AutomationLatches,
    gestures: &AutomationGestures,
) -> bool {
    let mut any_wrote = false;

    for ((target, param_id), preview) in previews.iter_mut() {
        let Some(wrote) = project.preset_instance_mut(target).map(|fx| {
            if !fx.enabled {
                return false;
            }
            if preview.lane.points.is_empty() {
                return false;
            }

            // A disabled arrangement lane remains disabled while a preview is
            // active. The draft must not silently turn an explicitly disabled
            // lane back on.
            if fx
                .automation_lanes
                .as_ref()
                .and_then(|lanes| lanes.iter().find(|lane| lane.param_id == *param_id))
                .is_some_and(|lane| !lane.enabled)
            {
                return false;
            }

            let Some(param) = fx.params.get(param_id.as_ref()) else {
                return false;
            };
            if param.touched
                || contains_automation_key(latches, &fx.id, param_id)
                || contains_automation_key(gestures, &fx.id, param_id)
            {
                return false;
            }

            let raw = preview.lane.value_at(current_beat);
            let value = param.driven_value(raw);
            let changed = param.base != value || param.value != value;
            fx.set_base_param_from_automation(param_id.as_ref(), value);
            // The draft, not the arrangement lane, drives the param now:
            // physics holds the value written here instead of reading the lane.
            if let Some(param) = fx.params.get_mut(param_id.as_ref()) {
                param.automated = false;
            }
            changed
        }) else {
            continue;
        };
        any_wrote |= wrote;
    }

    any_wrote
}

/// Allocation-free inspection walk used by stopped and paused ticks. This is
/// deliberately separate from the recording walk: recording needs a
/// two-pass scratch area and owned gesture keys, while inspection only needs
/// to sample lanes and update an occasional manual latch.
fn evaluate_all_automation_inspection(
    project: &mut Project,
    current_beat: Beats,
    latches: &mut AutomationLatches,
    gestures: &AutomationGestures,
    export: bool,
) -> (bool, Vec<Box<dyn Command>>) {
    let mut any_wrote = false;

    for fx in project.settings.master_effects.iter_mut() {
        any_wrote |=
            evaluate_instance_automation_inspection(fx, current_beat, latches, gestures, export);
    }
    for layer in project.timeline.layers.iter_mut() {
        if let Some(effects) = &mut layer.effects {
            for fx in effects.iter_mut() {
                any_wrote |= evaluate_instance_automation_inspection(
                    fx,
                    current_beat,
                    latches,
                    gestures,
                    export,
                );
            }
        }
        if let Some(gp) = layer.gen_params_mut() {
            any_wrote |= evaluate_instance_automation_inspection(
                gp,
                current_beat,
                latches,
                gestures,
                export,
            );
        }
    }

    // `Vec::new` has no backing allocation. Inspection never creates command
    // objects because it cannot close a recording gesture.
    (any_wrote, Vec::new())
}

/// Sample one instance without scratch vectors, cloned lane keys, recording,
/// or gesture closure. The only owned key operation is on a live touch, which
/// is an authoring event rather than the steady-state tick path.
fn evaluate_instance_automation_inspection(
    fx: &mut PresetInstance,
    current_beat: Beats,
    latches: &mut AutomationLatches,
    gestures: &AutomationGestures,
    export: bool,
) -> bool {
    if !fx.enabled {
        return false;
    }
    fx.ensure_base_values();
    let Some(lanes) = fx.automation_lanes.as_ref() else {
        return false;
    };
    let fx_id = &fx.id;
    let mut any_wrote = false;

    for lane in lanes.iter().filter(|lane| lane.enabled) {
        let Some(param) = fx.params.get(lane.param_id.as_ref()) else {
            continue;
        };
        if !export && param.touched {
            // Inspection treats a touch as the same manual override as an
            // unarmed playback tick. This preserves the hand's value while
            // the playhead is parked or being scrubbed, even when Arm is on.
            let param_id = lane.param_id.clone();
            if let Some(param) = fx.params.get_mut(param_id.as_ref()) {
                param.touched = false;
                param.automated = false;
            }
            latches.insert((fx_id.clone(), param_id), ());
            continue;
        }
        if !export
            && (contains_automation_key(latches, fx_id, &lane.param_id)
                || contains_automation_key(gestures, fx_id, &lane.param_id))
        {
            if let Some(param) = fx.params.get_mut(lane.param_id.as_ref()) {
                param.automated = false;
            }
            continue;
        }

        let raw = lane.value_at(current_beat);
        let value = param.driven_value(raw);
        let Some(param) = fx.params.get_mut(lane.param_id.as_ref()) else {
            continue;
        };
        any_wrote |= param.base != value || param.value != value;
        param.base = value;
        param.value = value;
        param.automated = true;
    }

    any_wrote
}

#[inline]
pub(crate) fn contains_automation_key<T>(
    map: &AHashMap<(EffectId, ParamId), T>,
    fx_id: &EffectId,
    param_id: &str,
) -> bool {
    map.keys()
        .any(|(key_fx_id, key_param_id)| key_fx_id == fx_id && key_param_id.as_ref() == param_id)
}

/// Evaluate every automation lane on a single instance, plus (when armed)
/// any touched param that has no lane yet at all — the "arm creates a lane"
/// path (section 5). Returns true if any lane wrote a value or recorded a point.
/// `beats` is (sampled beat, the frame's exact beat): lanes read the first,
/// touches record the second.
fn evaluate_instance_automation(
    fx: &mut PresetInstance,
    target: &GraphTarget,
    (current_beat, frame_beat): (Beats, Beats),
    latches: &mut AutomationLatches,
    write: &AutomationWrite<'_>,
    gestures: &mut AutomationGestures,
) -> bool {
    if !fx.enabled {
        return false;
    }
    let recording = write.armed;
    let has_lanes = fx.automation_lanes.as_ref().is_some_and(|v| !v.is_empty());
    // A param writes this frame when a hand touched it or, while armed, a
    // pointer holds it (still or not).
    let writing =
        |p: &manifold_core::params::Param| p.touched || (recording && write.held(target, p.id()));
    // Nothing to do unless there's an existing lane to sample/latch/continue
    // recording, or (armed) a hand that might be starting a brand new
    // gesture — see docs/AUTOMATION_LANES_DESIGN.md section 5 "arm creates a lane".
    if !(has_lanes || (recording && fx.params.iter().any(writing))) {
        return false;
    }
    let fx_id = fx.id.clone();

    // Pass 1 (immutable): classify every enabled lane's param — record (armed
    // hand), latch (unarmed touch), stay latched, wait inside an open take,
    // or sample.
    let mut to_latch: Vec<ParamId> = Vec::new();
    let mut to_write: Vec<(ParamId, f32)> = Vec::new();
    let mut to_record: Vec<(ParamId, f32)> = Vec::new();

    if has_lanes {
        let lanes = fx.automation_lanes.as_ref().unwrap();
        for lane in lanes.iter().filter(|l| l.enabled) {
            // Resolve directly against the id-keyed manifest — range +
            // integral-ness ride each `Param.spec`, no registry consultation.
            let Some(p) = fx.params.get(lane.param_id.as_ref()) else {
                continue;
            };
            if writing(p) {
                if recording {
                    to_record.push((lane.param_id.clone(), p.base));
                } else {
                    to_latch.push(lane.param_id.clone());
                }
                continue;
            }
            if contains_automation_key(gestures, &fx_id, &lane.param_id) {
                // Waiting within the gesture's inactivity grace window —
                // nothing new to record this frame, and NOT a sample
                // (recording owns this param until the gesture closes).
                continue;
            }
            if contains_automation_key(latches, &fx_id, &lane.param_id) {
                continue; // still overridden
            }
            // BUG-039: a periodic param (e.g. a rotation the performer
            // authored a multi-turn ramp on, points climbing past `max`)
            // wraps back into range on read instead of clamping and
            // plateauing at the rail — the stored `points` are untouched,
            // only this per-frame interpretation changes.
            let raw = lane.value_at(current_beat);
            let value = p.driven_value(raw);
            to_write.push((lane.param_id.clone(), value));
        }
    }

    // Pass over written params NOT covered by an existing enabled lane
    // above (armed only): this is how a lane is BORN from performance (section 5).
    // Includes a currently-disabled lane's param, which the gesture-start
    // lookup below treats the same as "not currently automated" from the
    // touch's perspective.
    if recording {
        let lanes = fx.automation_lanes.as_deref().unwrap_or_default();
        for p in fx.params.iter() {
            if !writing(p)
                || lanes
                    .iter()
                    .any(|l| l.enabled && l.param_id.as_ref() == p.id())
            {
                continue;
            }
            to_record.push((ParamId::from(p.id().to_string()), p.base));
        }
    }

    // Pass 2 (mutable): clear the touched flag + latch for params that were
    // just touched, then write sampled values via the non-touching
    // automation write path (using `set_base_param` here would set
    // `touched` on our own write and self-latch the very next frame — see
    // `docs/AUTOMATION_LANES_DESIGN.md` section 4), then feed recording gestures.
    for param_id in to_latch {
        if let Some(p) = fx.params.get_mut(param_id.as_ref()) {
            p.touched = false;
        }
        latches.insert((fx_id.clone(), param_id), ());
    }
    // Only a lane that writes this pass marks its param automated; a latch,
    // a take or a held hand leaves the frame's value authoritative.
    for lane in fx.automation_lanes.iter().flatten() {
        if let Some(p) = fx.params.get_mut(lane.param_id.as_ref()) {
            p.automated = false;
        }
    }
    let mut any_wrote = false;
    for (param_id, value) in to_write {
        let changed = fx
            .params
            .get(param_id.as_ref())
            .is_some_and(|p| p.base != value || p.value != value);
        fx.set_base_param_from_automation(param_id.as_ref(), value);
        if let Some(p) = fx.params.get_mut(param_id.as_ref()) {
            p.automated = true;
        }
        any_wrote |= changed;
    }
    for (param_id, applied_base) in to_record {
        if let Some(p) = fx.params.get_mut(param_id.as_ref()) {
            p.touched = false;
        }
        // Recording supersedes any stale override latch from before arm was
        // engaged — once the hand is recording, the param isn't "overridden"
        // in the Back-to-Arrangement sense, it's authoring new automation.
        if contains_automation_key(latches, &fx_id, &param_id) {
            latches.remove(&(fx_id.clone(), param_id.clone()));
        }

        if !contains_automation_key(gestures, &fx_id, &param_id) {
            let param = fx.params.get(param_id.as_ref());
            let (lo, hi) = param.map_or((0.0, 1.0), |p| p.slider_span());
            let pre_points = fx
                .automation_lanes
                .as_ref()
                .and_then(|lanes| lanes.iter().find(|l| l.param_id == param_id))
                .map(|lane| lane.points.clone());
            let gesture = GestureState::new(
                target.clone(),
                param_id.clone(),
                pre_points,
                (hi - lo).abs(),
                param.is_some_and(|p| p.whole_numbers()),
                current_beat,
            );
            gestures.insert((fx_id.clone(), param_id.clone()), gesture);
        }
        let Some(gesture) = gestures
            .iter_mut()
            .find(|((key_fx_id, key_param_id), _)| *key_fx_id == fx_id && *key_param_id == param_id)
            .map(|(_, gesture)| gesture)
        else {
            continue;
        };
        let shape = if gesture.stepped {
            SegmentShape::Hold
        } else {
            SegmentShape::Linear
        };
        gesture.record(
            AutomationPoint {
                beat: frame_beat,
                value: applied_base,
                shape,
            },
            current_beat,
        );
        any_wrote = true;
    }
    any_wrote
}

/// Close every gesture whose take ends this tick ([`GestureState::ends`]):
/// thin its last pass, join it onto the curve (old points before punch-in +
/// the recorded segment + old points after the last touch, with restricted
/// shapes preserving the sampled tails — "the old curve resumes exactly",
/// section 5) and return one `CommitRecordedGestureCommand` per closed
/// gesture for the caller to run through `EditingService`.
fn close_expired_gestures(
    gestures: &mut AutomationGestures,
    current_beat: Beats,
    write: &AutomationWrite<'_>,
) -> Vec<Box<dyn Command>> {
    let expired: Vec<(EffectId, ParamId)> = gestures
        .iter()
        .filter(|((_, param_id), g)| g.ends(param_id, current_beat, write))
        .map(|(key, _)| key.clone())
        .collect();
    expired
        .into_iter()
        .filter_map(|key| {
            gestures
                .remove(&key)
                .map(|gesture| gesture.into_command(key.1))
        })
        .collect()
}

/// Close every take at an explicit transport/save boundary, using the same
/// command and punch join as inactivity expiry. Draining is idempotent.
pub fn finish_all_gestures(gestures: &mut AutomationGestures) -> Vec<Box<dyn Command>> {
    gestures
        .drain()
        .map(|((_, param_id), gesture)| gesture.into_command(param_id))
        .collect()
}

/// Replace the recorded closed interval while keeping the old curve outside it.
/// Adjacent representable beats encode the two discontinuities without duplicate
/// point identities. Restricted curve shapes preserve interpolation on both tails.
/// Shared by recorded gestures and authored pattern insertion.
pub fn punch_recorded_points(
    original: &[AutomationPoint],
    recorded: &[AutomationPoint],
) -> Vec<AutomationPoint> {
    let old = AutomationLane {
        param_id: "punch".into(),
        enabled: true,
        points: original.to_vec(),
    };
    let mut points = Vec::with_capacity(original.len() + recorded.len() + 2);
    punch_recorded_points_into(&old, recorded, &mut points);
    points
}

/// [`punch_recorded_points`] into a reused buffer: `out` is cleared, and grows
/// only when the joined curve outgrows its capacity.
fn punch_recorded_points_into(
    old: &AutomationLane,
    recorded: &[AutomationPoint],
    out: &mut Vec<AutomationPoint>,
) {
    out.clear();
    let original = old.points.as_slice();
    let (Some(first), Some(last)) = (recorded.first(), recorded.last()) else {
        out.extend_from_slice(original);
        return;
    };
    if original.is_empty() {
        out.extend_from_slice(recorded);
        return;
    }
    let before = Beats(first.beat.0.next_down());
    let after = Beats(last.beat.0.next_up());
    out.extend(original.iter().filter(|p| p.beat < before).copied());
    if first.beat > Beats::ZERO {
        if let Some(left) = out.last_mut()
            && let Some(right) = original.iter().find(|p| p.beat > left.beat)
        {
            let t =
                ((before.0 - left.beat.0) / (right.beat.0 - left.beat.0)).clamp(0.0, 1.0) as f32;
            left.shape = left.shape.subrange(0.0, t);
        }
        out.push(AutomationPoint {
            beat: before,
            value: old.value_at(before),
            shape: SegmentShape::Hold,
        });
    }
    out.extend_from_slice(recorded);
    let shape = original
        .iter()
        .enumerate()
        .rfind(|(_, p)| p.beat <= after)
        .and_then(|(i, left)| {
            original.get(i + 1).map(|right| {
                let t =
                    ((after.0 - left.beat.0) / (right.beat.0 - left.beat.0)).clamp(0.0, 1.0) as f32;
                left.shape.subrange(t, 1.0)
            })
        })
        .unwrap_or(SegmentShape::Hold);
    out.push(AutomationPoint {
        beat: after,
        value: old.value_at(after),
        shape,
    });
    out.extend(original.iter().filter(|p| p.beat > after).copied());
}

// =====================================================================
// Tests. Mirrors `modulation::tests`'s fixture pattern: manifold-nodes
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
    use manifold_core::PresetTypeId;
    use manifold_core::effect_registration::EffectMetadata;
    use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};
    use manifold_core::generator_registration::ParamSpec;
    use manifold_core::layer::Layer;
    use manifold_core::preset_definition_registry::create_default;
    use manifold_core::project::Project;

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
    fn eval_unarmed(project: &mut Project, beat: Beats, latches: &mut AutomationLatches) -> bool {
        let mut gestures = AutomationGestures::default();
        let (wrote, commits) =
            evaluate_all_automation(project, beat, latches, false, &mut gestures);
        assert!(
            commits.is_empty(),
            "arm off must never produce a gesture commit"
        );
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
    fn inspection_samples_curve_without_dirtying_an_unchanged_value() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        let (wrote, commits) = evaluate_all_automation_with_mode(
            &mut project,
            Beats(2.0),
            &mut latches,
            false,
            &mut gestures,
            AutomationSamplingMode::Inspection,
        );
        assert!(wrote);
        assert!(commits.is_empty());
        assert_eq!(
            project.timeline.layers[0].effects.as_ref().unwrap()[0]
                .params
                .get("amount")
                .unwrap()
                .base,
            0.5
        );

        let (wrote, commits) = evaluate_all_automation_with_mode(
            &mut project,
            Beats(2.0),
            &mut latches,
            false,
            &mut gestures,
            AutomationSamplingMode::Inspection,
        );
        assert!(
            !wrote,
            "an unchanged inspection sample need not refresh output"
        );
        assert!(commits.is_empty());
    }

    #[test]
    fn inspection_never_records_or_closes_a_gesture() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        layer.effects.as_mut().unwrap()[0].set_base_param("amount", 0.42);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        // Establish an active recording gesture through the normal playback
        // mode, then seek well beyond its inactivity window in inspection.
        let (_, commits) = evaluate_all_automation_with_mode(
            &mut project,
            Beats(1.0),
            &mut latches,
            true,
            &mut gestures,
            AutomationSamplingMode::Playback,
        );
        assert!(commits.is_empty());
        assert_eq!(gestures.len(), 1);

        let (_, commits) = evaluate_all_automation_with_mode(
            &mut project,
            Beats(10.0),
            &mut latches,
            true,
            &mut gestures,
            AutomationSamplingMode::Inspection,
        );
        assert!(commits.is_empty(), "inspection seeks cannot close gestures");
        assert_eq!(gestures.len(), 1, "the active gesture remains in flight");
    }

    #[test]
    fn armed_inspection_touch_is_a_manual_override_not_recording() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        layer.effects.as_mut().unwrap()[0].set_base_param("amount", 0.91);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        let (wrote, commits) = evaluate_all_automation_with_mode(
            &mut project,
            Beats(2.0),
            &mut latches,
            true,
            &mut gestures,
            AutomationSamplingMode::Inspection,
        );
        assert!(!wrote);
        assert!(commits.is_empty());
        assert!(gestures.is_empty());
        assert_eq!(latches.len(), 1);
        assert_eq!(
            project.timeline.layers[0].effects.as_ref().unwrap()[0]
                .params
                .get("amount")
                .unwrap()
                .base,
            0.91
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
        assert_eq!(
            latches.len(),
            1,
            "the touch latches the param as overridden"
        );
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
        assert!(
            wrote,
            "automation keeps sampling across frames uninterrupted"
        );
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
        assert_eq!(
            fx.params.get("amount").unwrap().base,
            0.0,
            "untouched, still the default"
        );
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
        assert_eq!(
            project.timeline.layers[0].effects.as_ref().unwrap().len(),
            1
        );
    }

    #[test]
    fn generator_param_lane_samples_too() {
        // Walk shape must cover generator params, same as
        // `evaluate_all_audio_mods` — not just layer/master effects.
        let mut layer = Layer::new_generator("GenLayer".into(), TEST_FX, 0);
        layer
            .gen_params_mut()
            .unwrap()
            .init_defaults_for_type(TEST_FX);
        layer.gen_params_mut().unwrap().automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();

        let wrote = eval_unarmed(&mut project, Beats(4.0), &mut latches);
        assert!(wrote);
        let gp = project.timeline.layers[0].gen_params().unwrap();
        assert!((gp.params.get("amount").unwrap().base - 0.8).abs() < 1e-6);
    }

    /// A draft lane being dragged writes the param, so physics must hold that
    /// value rather than read the arrangement lane underneath it.
    #[test]
    fn preview_leaves_the_param_unautomated() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        eval_unarmed(&mut project, Beats(2.0), &mut latches);
        let fx = &project.timeline.layers[0].effects.as_ref().unwrap()[0];
        assert!(
            fx.params.get("amount").unwrap().automated,
            "the arrangement lane drives the param"
        );
        let mut draft = amount_lane();
        draft.points[1].value = 0.2;
        let mut previews = AutomationLanePreviews::default();
        previews.insert(
            (
                manifold_core::GraphTarget::Effect(fx.id.clone()),
                "amount".into(),
            ),
            AutomationLanePreview {
                lane: draft,
                original_base: 0.5,
            },
        );
        assert!(evaluate_automation_previews(
            &mut project,
            Beats(2.0),
            &mut previews,
            &latches,
            &AutomationGestures::default()
        ));
        let amount = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap();
        assert_eq!(amount.base, 0.2);
        assert!(
            !amount.automated,
            "a previewed param is held, not read from the arrangement lane"
        );
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
        // "Arm creates a lane" (section 5): no lane exists yet for `amount`, but a
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
        assert_eq!(
            commits.len(),
            1,
            "the gesture closes after ~2 beats of inactivity"
        );
        assert!(gestures.is_empty());

        let mut cmd = commits.into_iter().next().unwrap();
        let fx_id = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .id
            .clone();
        cmd.execute(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        let lane = fx
            .automation_lanes
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.param_id.as_ref() == "amount")
            .unwrap();
        assert_eq!(
            lane.points.len(),
            1,
            "one recorded point at the punch-in beat"
        );
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
        assert!(
            wrote,
            "still samples normally — no live touch, so no gesture"
        );
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
        let fx_id = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .id
            .clone();
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();

        // Touch at beat 1, recording a new value over the existing curve.
        project.timeline.layers[0].effects.as_mut().unwrap()[0].set_base_param("amount", 0.6);
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
        assert!(
            lane.points
                .iter()
                .any(|p| p.beat.0 == 0.0 && (p.value - 0.2).abs() < 1e-6)
        );
        assert!(
            lane.points
                .iter()
                .any(|p| p.beat.0 == 1.0 && (p.value - 0.6).abs() < 1e-6)
        );
        assert!(
            lane.points
                .iter()
                .any(|p| p.beat.0 == 4.0 && (p.value - 0.8).abs() < 1e-6)
        );

        cmd.undo(&mut project);
        let fx = project.find_effect_by_id(&fx_id).unwrap();
        let lane = fx
            .automation_lanes
            .as_ref()
            .unwrap()
            .iter()
            .find(|l| l.param_id.as_ref() == "amount")
            .unwrap();
        assert_eq!(
            lane.points.len(),
            2,
            "undo restores the exact pre-gesture point set"
        );
        assert!((lane.points[0].value - 0.2).abs() < 1e-6);
        assert!((lane.points[1].value - 0.8).abs() < 1e-6);
    }

    #[test]
    fn recording_punch_preserves_dense_samples_outside_take() {
        for shape in [
            SegmentShape::Linear,
            SegmentShape::Hold,
            SegmentShape::Curved(0.7),
            SegmentShape::Curved(-0.8),
        ] {
            let original = AutomationLane {
                param_id: "amount".into(),
                enabled: true,
                points: vec![
                    AutomationPoint {
                        beat: Beats(0.0),
                        value: 0.0,
                        shape,
                    },
                    AutomationPoint {
                        beat: Beats(8.0),
                        value: 1.0,
                        shape: SegmentShape::Linear,
                    },
                ],
            };
            for (start, end) in [(2.0, 2.0), (2.0, 5.0), (0.0, 3.0), (9.0, 10.0)] {
                let mut recorded = vec![AutomationPoint {
                    beat: Beats(start),
                    value: 0.9,
                    shape: SegmentShape::Linear,
                }];
                if end != start {
                    recorded.push(AutomationPoint {
                        beat: Beats(end),
                        value: 0.3,
                        shape: SegmentShape::Linear,
                    });
                }
                let changed = AutomationLane {
                    points: punch_recorded_points(&original.points, &recorded),
                    ..original.clone()
                };
                assert!(changed.points.windows(2).all(|w| w[0].beat < w[1].beat));
                assert_eq!(changed.value_at(Beats(start)), 0.9);
                for i in 0..1100 {
                    let beat = Beats(i as f64 / 100.0);
                    if beat.0 >= start && beat.0 <= end {
                        continue;
                    }
                    assert!(
                        (changed.value_at(beat) - original.value_at(beat)).abs() < 2e-6,
                        "outside punch changed at {beat:?}, {shape:?}, {start}..{end}"
                    );
                }
            }
        }
    }

    /// A loop wrap or backward seek starts a new pass: it overwrites only the
    /// beats it covers, and the earlier pass survives past it (Logic / Pro
    /// Tools cycle recording), all in one undo entry.
    #[test]
    fn recording_backward_pass_overwrites_only_the_beats_it_covers() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let original = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0]
            .clone();
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();
        for (beat, value) in [(3.0, 0.9), (3.5, 0.8), (1.0, 0.7), (2.0, 0.6)] {
            project.timeline.layers[0].effects.as_mut().unwrap()[0].set_base_param("amount", value);
            let (_, commits) = evaluate_all_automation(
                &mut project,
                Beats(beat),
                &mut latches,
                true,
                &mut gestures,
            );
            assert!(commits.is_empty());
        }
        let mut commits = finish_all_gestures(&mut gestures);
        assert_eq!(commits.len(), 1);
        commits[0].execute(&mut project);
        let lane = &project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0];
        assert!(lane.points.windows(2).all(|p| p[0].beat < p[1].beat));
        assert_eq!(lane.value_at(Beats(1.0)), 0.7);
        assert_eq!(lane.value_at(Beats(2.0)), 0.6);
        assert_eq!(
            lane.value_at(Beats(3.0)),
            0.9,
            "the first pass survives past the second"
        );
        assert_eq!(lane.value_at(Beats(3.5)), 0.8);
        for beat in [0.5, 2.5, 4.0] {
            assert!((lane.value_at(Beats(beat)) - original.value_at(Beats(beat))).abs() < 1e-6);
        }
        commits[0].undo(&mut project);
        assert_eq!(
            project.timeline.layers[0].effects.as_ref().unwrap()[0]
                .automation_lanes
                .as_ref()
                .unwrap()[0]
                .points,
            original.points
        );
    }

    #[test]
    fn recording_flush_and_export_preserve_live_ownership() {
        let mut layer = layer_with_one_effect();
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![amount_lane()]);
        let mut project = project_with(layer);
        let mut latches = AutomationLatches::default();
        let mut gestures = AutomationGestures::default();
        project.timeline.layers[0].effects.as_mut().unwrap()[0].set_base_param("amount", 0.9);
        let _ =
            evaluate_all_automation(&mut project, Beats(2.0), &mut latches, true, &mut gestures);
        let mut commands = finish_all_gestures(&mut gestures);
        assert_eq!(commands.len(), 1);
        assert!(finish_all_gestures(&mut gestures).is_empty());
        let original = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0]
            .points
            .clone();
        commands[0].execute(&mut project);
        let lane = &project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0];
        assert!((lane.value_at(Beats(1.0)) - 0.35).abs() < 1e-6);
        assert_eq!(lane.value_at(Beats(2.0)), 0.9);
        commands[0].undo(&mut project);
        assert_eq!(
            project.timeline.layers[0].effects.as_ref().unwrap()[0]
                .automation_lanes
                .as_ref()
                .unwrap()[0]
                .points,
            original
        );
        let fx = &mut project.timeline.layers[0].effects.as_mut().unwrap()[0];
        fx.set_base_param("amount", 0.95);
        latches.insert((fx.id.clone(), "amount".into()), ());
        let (_, commits) = evaluate_all_automation_with_mode(
            &mut project,
            Beats(2.0),
            &mut latches,
            true,
            &mut gestures,
            AutomationSamplingMode::Export,
        );
        let p = project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .params
            .get("amount")
            .unwrap();
        assert!((p.value - 0.5).abs() < 1e-6);
        assert!(p.touched);
        assert_eq!(latches.len(), 1);
        assert!(gestures.is_empty() && commits.is_empty());
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
        assert!(
            commits.is_empty(),
            "under the ~2 beat threshold, the gesture stays open"
        );
        assert_eq!(gestures.len(), 1);
    }

    // ── Write modes and thinning ────────────────────────────────────────

    fn eval_write(
        project: &mut Project,
        beat: f64,
        latches: &mut AutomationLatches,
        gestures: &mut AutomationGestures,
        write: AutomationWrite<'_>,
    ) -> Vec<Box<dyn Command>> {
        evaluate_all_automation_at(
            project,
            Beats(beat),
            Beats(beat),
            latches,
            write,
            gestures,
            AutomationSamplingMode::Playback,
        )
        .1
    }

    fn fx_mut(project: &mut Project) -> &mut PresetInstance {
        &mut project.timeline.layers[0].effects.as_mut().unwrap()[0]
    }

    fn amount_points(project: &Project) -> AutomationLane {
        project.timeline.layers[0].effects.as_ref().unwrap()[0]
            .automation_lanes
            .as_ref()
            .unwrap()[0]
            .clone()
    }

    /// 0.2 at beat 0 rising to 0.8 at beat 8.
    fn long_lane_project() -> Project {
        let mut layer = layer_with_one_effect();
        let mut lane = amount_lane();
        lane.points[1].beat = Beats(8.0);
        layer.effects.as_mut().unwrap()[0].automation_lanes = Some(vec![lane]);
        project_with(layer)
    }

    fn point(beat: f64, value: f32) -> AutomationPoint {
        AutomationPoint {
            beat: Beats(beat),
            value,
            shape: SegmentShape::Linear,
        }
    }

    /// One whole pass through the recorder's streaming thinner.
    fn thin_recorded_points(points: &mut Vec<AutomationPoint>, range: f32, stepped: bool) {
        let raw = std::mem::take(points);
        let mut thinner = PassThinner::new(range, stepped);
        for p in raw {
            thinner.push(points, p);
        }
    }

    fn assert_within_tolerance(thinned: &[AutomationPoint], played: &[AutomationPoint]) {
        let lane = AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: thinned.to_vec(),
        };
        for p in played {
            let error = (lane.value_at(p.beat) - p.value).abs();
            assert!(
                f64::from(error) <= THINNING_TOLERANCE + 1e-6,
                "error {error} at {:?}",
                p.beat
            );
        }
    }

    /// A five-minute take at 60 fps records through the real recorder: the
    /// lane stays within tolerance of every frame played and keeps far fewer
    /// points.
    #[test]
    fn long_take_thins_within_tolerance() {
        let frames: Vec<_> = (0..18_000)
            .map(|i| {
                let t = i as f32;
                let value = if (6_000..9_000).contains(&i) {
                    0.7
                } else {
                    0.5 + 0.3 * (t * 0.004).sin() + 0.1 * (t * 0.02).sin()
                };
                point(f64::from(i) / 30.0, value)
            })
            .collect();
        let target = GraphTarget::Effect(EffectId::new("fx"));
        let mut take = GestureState::new(target, "amount".into(), None, 1.0, false, Beats::ZERO);
        for p in &frames {
            take.record(*p, p.beat);
        }
        take.finish_pass();
        let lane = &take.layered.points;
        assert_eq!(
            (lane[0], *lane.last().unwrap()),
            (frames[0], frames[17_999])
        );
        assert!(
            lane.len() < frames.len() / 10,
            "kept {} of {}",
            lane.len(),
            frames.len()
        );
        assert_within_tolerance(lane, &frames);
    }

    /// Two frames on one beat: the newer value wins, and the points already
    /// dropped stay within tolerance even when the newer value breaks their
    /// line.
    #[test]
    fn thinning_a_repeated_beat_keeps_the_newest_value() {
        let ramp: Vec<_> = (0..=20)
            .map(|i| point(f64::from(i) * 0.1, i as f32 * 0.01))
            .collect();
        let jump = point(2.0, 0.9);
        let mut thinned = ramp.clone();
        thinned.push(jump);
        thin_recorded_points(&mut thinned, 1.0, false);
        assert_eq!(*thinned.last().unwrap(), jump);
        assert!(
            thinned.windows(2).all(|w| w[0].beat < w[1].beat),
            "beats stay strictly rising"
        );
        assert_within_tolerance(&thinned[..thinned.len() - 1], &ramp[..20]);
    }

    #[test]
    fn thinning_keeps_endpoints_and_stays_within_tolerance() {
        let ramp: Vec<_> = (0..=100)
            .map(|i| point(i as f64 * 0.04, i as f32 / 100.0))
            .collect();
        let mut thinned = ramp.clone();
        thin_recorded_points(&mut thinned, 1.0, false);
        assert_eq!(
            thinned,
            vec![ramp[0], ramp[100]],
            "a straight ramp needs only its endpoints"
        );

        // A sine sweep: thinning drops most points, keeps both ends, and the
        // thinned curve never strays past the tolerance from any sample.
        let sweep: Vec<_> = (0..=400)
            .map(|i| point(i as f64 * 0.01, 0.5 + 0.4 * (i as f32 * 0.03).sin()))
            .collect();
        let mut thinned = sweep.clone();
        thin_recorded_points(&mut thinned, 1.0, false);
        assert!(
            thinned.len() < sweep.len() / 4,
            "kept {} of {}",
            thinned.len(),
            sweep.len()
        );
        assert_eq!(
            (thinned[0], *thinned.last().unwrap()),
            (sweep[0], sweep[400])
        );
        let lane = AutomationLane {
            param_id: "amount".into(),
            enabled: true,
            points: thinned,
        };
        for p in &sweep {
            let error = (lane.value_at(p.beat) - p.value).abs();
            assert!(
                f64::from(error) <= THINNING_TOLERANCE + 1e-6,
                "error {error} at {:?}",
                p.beat
            );
        }
    }

    #[test]
    fn thinning_a_stepped_param_keeps_only_value_changes() {
        let hold = |beat, value| AutomationPoint {
            beat: Beats(beat),
            value,
            shape: SegmentShape::Hold,
        };
        let mut points = vec![
            hold(0.0, 1.0),
            hold(0.1, 1.0),
            hold(0.2, 2.0),
            hold(0.3, 2.0),
            hold(0.4, 2.0),
            hold(0.5, 1.0),
            hold(0.6, 1.0),
        ];
        thin_recorded_points(&mut points, 4.0, true);
        assert_eq!(
            points,
            vec![
                hold(0.0, 1.0),
                hold(0.2, 2.0),
                hold(0.5, 1.0),
                hold(0.6, 1.0)
            ]
        );
    }

    #[test]
    fn held_still_keeps_writing_and_release_ends_the_take_at_once() {
        let mut project = long_lane_project();
        let original = amount_points(&project);
        let fx_id = fx_mut(&mut project).id.clone();
        let holds = vec![(GraphTarget::Effect(fx_id), ParamId::from("amount"))];
        let held = AutomationWrite {
            armed: true,
            holds: &holds,
        };
        let (mut latches, mut gestures) =
            (AutomationLatches::default(), AutomationGestures::default());

        fx_mut(&mut project).set_base_param("amount", 0.3);
        assert!(eval_write(&mut project, 1.0, &mut latches, &mut gestures, held).is_empty());
        // Held still for four beats: no inactivity punch-out, and the lane
        // does not take the param back.
        for frame in 1..=240 {
            let commits = eval_write(
                &mut project,
                1.0 + frame as f64 / 60.0,
                &mut latches,
                &mut gestures,
                held,
            );
            assert!(commits.is_empty(), "a held control never times out");
        }
        assert_eq!(fx_mut(&mut project).params.get("amount").unwrap().base, 0.3);
        let take = gestures.values().next().unwrap();
        assert!(
            take.recorded.len() <= 2,
            "a still hold slides one point, got {}",
            take.recorded.len()
        );

        release_hold(&mut gestures, &holds[0].0, "amount");
        let commits = eval_write(
            &mut project,
            5.0,
            &mut latches,
            &mut gestures,
            AutomationWrite::unheld(true),
        );
        assert_eq!(commits.len(), 1, "release ends the take on the next tick");
        let mut command = commits.into_iter().next().unwrap();
        command.execute(&mut project);
        let lane = amount_points(&project);
        for beat in [1.0, 3.0, 5.0] {
            assert_eq!(
                lane.value_at(Beats(beat)),
                0.3,
                "held value written at {beat}"
            );
        }
        assert!((lane.value_at(Beats(6.0)) - original.value_at(Beats(6.0))).abs() < 1e-6);
        command.undo(&mut project);
        assert_eq!(amount_points(&project).points, original.points);
    }

    /// Release ends the take straight away, well inside the inactivity
    /// window that inputs with no release still use.
    #[test]
    fn release_ends_the_take_before_the_inactivity_timeout() {
        let mut project = long_lane_project();
        let fx_id = fx_mut(&mut project).id.clone();
        let holds = vec![(GraphTarget::Effect(fx_id), ParamId::from("amount"))];
        let held = AutomationWrite {
            armed: true,
            holds: &holds,
        };
        let (mut latches, mut gestures) =
            (AutomationLatches::default(), AutomationGestures::default());
        fx_mut(&mut project).set_base_param("amount", 0.6);
        assert!(eval_write(&mut project, 1.0, &mut latches, &mut gestures, held).is_empty());
        release_hold(&mut gestures, &holds[0].0, "amount");
        let commits = eval_write(
            &mut project,
            1.25,
            &mut latches,
            &mut gestures,
            AutomationWrite::unheld(true),
        );
        assert_eq!(commits.len(), 1);
        assert!(gestures.is_empty());
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
                AutomationPoint {
                    beat: Beats(0.0),
                    value: 0.0,
                    shape: SegmentShape::Linear,
                },
                // A continuous multi-turn ramp: 720 = two full rotations.
                AutomationPoint {
                    beat: Beats(4.0),
                    value: 720.0,
                    shape: SegmentShape::Linear,
                },
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
                AutomationPoint {
                    beat: Beats(0.0),
                    value: 0.0,
                    shape: SegmentShape::Linear,
                },
                AutomationPoint {
                    beat: Beats(4.0),
                    value: 720.0,
                    shape: SegmentShape::Linear,
                },
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
