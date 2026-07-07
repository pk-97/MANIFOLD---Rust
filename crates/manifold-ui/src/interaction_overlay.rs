//! Single transparent overlay covering the entire tracks area.
//! Centralises all clip interaction (click, hover, drag, trim, box-select).
//!
//! Mechanical translation of Assets/Scripts/UI/Timeline/InteractionOverlay.cs.
//!
//! All interaction routing goes through this struct. The viewport panel becomes
//! purely rendering + coordinate conversion. The overlay calls through the
//! `TimelineEditingHost` trait for operations that need engine/editing access.

use manifold_foundation::{Beats, ClipId, ParamId, Seconds};
use std::collections::HashSet;

use crate::anim::{AnimF32, Transient};
use crate::automation_hit_tester::{self, AutomationHit};
use crate::clip_hit_tester::{ClipHitResult, ClipHitTester, HitRegion};
use crate::color;
use crate::input::Modifiers;
use crate::node::Vec2;
use crate::panels::viewport::TimelineViewportPanel;
use crate::timeline_editing_host::{ClipRef, TimelineCursor, TimelineEditingHost};
use crate::ui_state::UIState;
use crate::view::{UiAutomationPointRef, UiGraphTarget, UiSegmentShape};

// ── Constants ───────────────────────────────────────────────────
// Unity InteractionOverlay lines 78-79.

// Note: SNAP_THRESHOLD_PX and MAX_SNAP_BEATS live on TimelineViewportPanel
// (viewport.rs magnetic_snap). These overlay constants will be needed when
// overlay-level snapping is ported (Unity InteractionOverlay lines 78-79).

// ── Shift+Click region selection ─────────────────────────────────
// Port of Unity EditingService.SelectRegionTo (lines 216-262).
// Free function because it needs both UIState and host access.

/// Shift+Click region selection with correct anchor precedence.
/// Anchor priority: insert cursor > existing region > primary selected clip > fallback.
fn select_region_to(
    target_beat: Beats,
    target_layer: usize,
    ui_state: &mut UIState,
    host: &dyn TimelineEditingHost,
) {
    let layer_count = host.layer_count();
    if layer_count == 0 {
        return;
    }

    // Determine anchor — Unity priority: insert cursor > region > primary clip > fallback
    let anchor: Option<(Beats, usize)> = if ui_state.has_insert_cursor() {
        // Resolve insert cursor layer_id back to an index for region computation
        let anchor_idx = ui_state
            .insert_cursor_layer_id
            .as_ref()
            .and_then(|id| {
                (0..layer_count).find(|&i| host.layer_id_at_index(i).as_ref() == Some(id))
            })
            .unwrap_or(0);
        Some((
            ui_state.insert_cursor_beat.unwrap_or(Beats::ZERO),
            anchor_idx,
        ))
    } else if let Some(r) = ui_state.current_region() {
        let start_idx = r
            .layer_index_range(host.layers())
            .map(|(lo, _)| lo)
            .unwrap_or(0);
        Some((r.start_beat, start_idx))
    } else if let Some(clip_id) = ui_state.primary_selected_clip_id.clone() {
        host.find_clip_by_id(&clip_id)
            .map(|c| (c.start_beat, c.layer_index))
    } else {
        None
    };

    match anchor {
        Some((anchor_beat, anchor_layer)) => {
            let min_beat = anchor_beat.min(target_beat);
            let max_beat = anchor_beat.max(target_beat);
            let min_layer = anchor_layer.min(target_layer).min(layer_count - 1) as i32;
            let max_layer = anchor_layer.max(target_layer).min(layer_count - 1) as i32;
            ui_state.set_region(min_beat, max_beat, min_layer, max_layer, host.layers());
        }
        None => {
            // No anchor — set insert cursor at target (Unity line 247-248)
            let layer_id = host.layer_id_at_index(target_layer).unwrap_or_default();
            ui_state.set_insert_cursor(target_beat, layer_id);
        }
    }
}

// ── Shift+Click clip-range selection (D2) ──────────────────────────
// `docs/TIMELINE_INTERACTION_P1_SPEC.md` D2: shift-click on a CLIP is a
// contiguous whole-clip range selection, NOT a time-range region — the
// `select_region_to` call this arm used to make (same as the empty-lane
// shift path above) was S1/S3's root. This is the live-gesture twin of
// `ui_bridge::select_clip_range_to_with_project` (manifold-app), which does
// the identical thing against a `&Project` for the dispatch/test-harness
// surface; this one reads through `TimelineEditingHost` for the real
// `on_pointer_click` path. Both must move together — see the phase notes.

/// Shift+Click clip-range selection: extend from the current `Clips`
/// selection's anchor to `target_clip_id`, selecting every WHOLE clip on the
/// **anchor's** layer whose start beat falls between the anchor and the
/// target, inclusive. A gap between clips simply isn't a clip — nothing
/// synthesizes a region there. No live anchor (fresh selection, or the
/// anchor clip vanished) falls back to a plain single-clip select.
fn select_clip_range_to(target_clip_id: &str, ui_state: &mut UIState, host: &dyn TimelineEditingHost) {
    let Some(target) = host.find_clip_by_id(target_clip_id) else {
        return; // clip vanished under us
    };

    let anchor_id = ui_state.clip_selection_anchor();
    let anchor = anchor_id.as_ref().and_then(|id| host.find_clip_by_id(id));

    let Some(anchor) = anchor else {
        // No live anchor to extend from — behaves like a plain click on the target.
        ui_state.select_clip(ClipId::new(target_clip_id), target.layer_id);
        return;
    };

    let min_beat = anchor.start_beat.min(target.start_beat);
    let max_beat = anchor.start_beat.max(target.start_beat);

    let ids: HashSet<ClipId> = host
        .clips_on_layer(anchor.layer_index)
        .into_iter()
        .filter(|c| c.start_beat >= min_beat && c.start_beat <= max_beat)
        .map(|c| c.clip_id)
        .collect();

    ui_state.set_clip_range(
        ids,
        anchor_id.expect("anchor Some implies anchor_id Some"),
        ClipId::new(target_clip_id),
        target.layer_id,
    );
}

// ── DragMode ────────────────────────────────────────────────────
// Unity InteractionOverlay line 37.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    None,
    Move,
    TrimLeft,
    TrimRight,
    RegionSelect,
    /// Dragging an existing automation breakpoint (P4 Unit A,
    /// `docs/AUTOMATION_LANES_DESIGN.md` §7). State lives in
    /// `InteractionOverlay::automation_drag`, mirroring how `Move`'s state
    /// lives in `drag_snapshots`/`drag_start_beat`.
    AutomationPoint,
    /// Alt-dragging a segment into a curve bend (P4 Unit B, §7's
    /// "modifier-drag a segment"). State in `automation_segment_bend`.
    AutomationSegmentBend,
    /// Plain (no-Alt) vertical drag of a segment — both endpoints move by the
    /// same value delta (P4 Unit B, §7's "drag a segment"). State in
    /// `automation_segment_drag`.
    AutomationSegmentDrag,
    /// Rubber-band selecting automation breakpoints (P4 Unit B, §7's
    /// "Marquee-select multiple dots"). State in `automation_marquee`.
    AutomationMarquee,
    /// Dragging a marquee-selected GROUP of breakpoints together (grabbed one
    /// of the selected dots while the marquee set has 2+ members). State in
    /// `automation_group_drag`.
    AutomationGroupMove,
    /// Pencil/draw mode stroke (P4 Unit B, §7's "Draw mode") — active
    /// whenever `UIState::automation_draw_mode` is on and the press lands in
    /// a lane strip, overriding dot/segment/marquee routing entirely. State
    /// in `automation_draw`.
    AutomationDraw,
}

// ── AutomationDragState ─────────────────────────────────────────
// P4 Unit A (`docs/AUTOMATION_LANES_DESIGN.md` §7): grabbed-dot geometry for
// a `DragMode::AutomationPoint` drag. `last_beat`/`last_value` track where
// the point currently sits (recomputed fresh from screen each frame, not
// incrementally) so `TimelineEditingHost::set_automation_point_preview`'s
// by-beat lookup always finds it — the same "recompute from origin, not
// incrementally" discipline `handle_move_drag` uses for clips.

#[derive(Debug, Clone)]
struct AutomationDragState {
    target: UiGraphTarget,
    param_id: ParamId,
    /// The point's state at grab time, PARAM RANGE value — the explicit
    /// reverse `commit_automation_point_move` needs (the
    /// `EditParamMappingCommand::new_with_reverse` drag-commit precedent).
    original: (Beats, f32, UiSegmentShape),
    /// Where the preview last wrote the point — the `from_beat` the NEXT
    /// preview call must search by.
    last_beat: Beats,
    last_value: f32,
}

// ── AutomationSegmentBendState / AutomationSegmentDragState ────────
// P4 Unit B (`docs/AUTOMATION_LANES_DESIGN.md` §7): grabbed-segment geometry
// for the two segment gestures. Both re-derive their live value from the
// ORIGINAL grab geometry each frame (never incrementally), matching every
// other drag state in this file.

/// Alt-drag curve bend: only the LEAVING point's `shape` changes (beat/value
/// untouched), so this carries just enough to preview + commit that one
/// field. `grab_y` is the screen Y at press time; `bend` is re-derived from
/// the vertical delta since grab, not accumulated.
#[derive(Debug, Clone)]
struct AutomationSegmentBendState {
    target: UiGraphTarget,
    param_id: ParamId,
    left_beat: Beats,
    left_value: f32,
    original_shape: UiSegmentShape,
    grab_y: f32,
    last_bend: f32,
}

/// Plain vertical segment drag: both endpoints move by the same normalized
/// value delta. Carries both points' original (param-range) values and
/// shapes — shape is unchanged by this gesture but threaded through so the
/// commit's `MoveAutomationPointCommand`s preserve it exactly.
#[derive(Debug, Clone)]
struct AutomationSegmentDragState {
    target: UiGraphTarget,
    param_id: ParamId,
    left_beat: Beats,
    left_original_value: f32,
    left_shape: UiSegmentShape,
    right_beat: Beats,
    right_original_value: f32,
    right_shape: UiSegmentShape,
    /// Normalized (0..1) Y within the strip at press time.
    grab_norm: f32,
    last_left_value: f32,
    last_right_value: f32,
}

// ── AutomationMarqueeState / AutomationGroupDragState / AutomationDrawState
// ────────────────────────────────────────────────────────────────────────
// P4 Unit B (`docs/AUTOMATION_LANES_DESIGN.md` §7): marquee-select, group
// move of the marquee-selected set, and pencil/draw mode.

/// Rubber-band marquee in progress — just the press corner; the current
/// corner is always the live `pos` passed to `on_drag`.
#[derive(Debug, Clone, Copy)]
struct AutomationMarqueeState {
    start: Vec2,
}

/// One marquee-selected point's captured geometry for a group move. Each
/// point keeps its OWN param range (points can span multiple lanes/params at
/// once) — only the normalized delta is shared across the group.
#[derive(Debug, Clone)]
struct AutomationGroupPointState {
    target: UiGraphTarget,
    param_id: ParamId,
    beat: Beats,
    original_value: f32,
    shape: UiSegmentShape,
    param_min: f32,
    param_max: f32,
    last_value: f32,
}

/// Dragging the whole marquee-selected group. `grab_target`/`grab_param_id`
/// identify the GRABBED lane, re-resolved fresh each frame for its
/// `strip_rect` (handles mid-drag scroll) to compute the shared normalized
/// delta applied to every point in `points`.
#[derive(Debug, Clone)]
struct AutomationGroupDragState {
    grab_target: UiGraphTarget,
    grab_param_id: ParamId,
    grab_norm: f32,
    points: Vec<AutomationGroupPointState>,
}

/// One in-progress pencil/draw stroke. `old_points` is the FULL (unfiltered
/// by visible range) pre-stroke point list — `None` if the stroke is
/// creating the lane — captured via `TimelineEditingHost::
/// automation_lane_points` at grab time, since `AutomationLaneScreen::dots`
/// is culled to the visible beat range and would silently drop off-screen
/// points. `working` is the list being built as the stroke proceeds; it
/// starts as a clone of `old_points` (or empty).
#[derive(Debug, Clone)]
struct AutomationDrawState {
    target: UiGraphTarget,
    param_id: ParamId,
    /// Value/beat denormalization always re-resolves the lane's CURRENT
    /// `param_min`/`param_max` fresh each frame in `write_automation_draw_step`
    /// (handles the (practically static, but consistent with every other
    /// handler's discipline) case of the range changing mid-stroke) — not
    /// cached here.
    new_point_shape: UiSegmentShape,
    old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    working: Vec<(Beats, f32, UiSegmentShape)>,
}

/// Insert-or-overwrite `(beat, value, shape)` into a sorted-by-beat working
/// point list — the pencil's per-grid-step write. Matches an existing beat
/// exactly (grid-snapped beats are stable across frames within the same
/// grid cell) or inserts at the sorted position.
fn apply_draw_point(points: &mut Vec<(Beats, f32, UiSegmentShape)>, beat: Beats, value: f32, shape: UiSegmentShape) {
    match points.iter().position(|p| p.0.0 == beat.0) {
        Some(idx) => points[idx] = (beat, value, shape),
        None => {
            let pos = points.iter().position(|p| p.0.0 > beat.0).unwrap_or(points.len());
            points.insert(pos, (beat, value, shape));
        }
    }
}

// ── DragSnapshot ────────────────────────────────────────────────
// Unity InteractionOverlay lines 49-54.

#[derive(Debug, Clone)]
pub struct DragSnapshot {
    pub clip_id: ClipId,
    pub start_beat: Beats,
    pub layer_index: usize,
}

// ── TrimOriginal ────────────────────────────────────────────────
// Per-clip pre-trim geometry captured at grab time. A trim drag fans the
// grabbed clip's edge delta over one of these per selected clip, then
// records each into a single undo batch on drag end.

#[derive(Debug, Clone)]
struct TrimOriginal {
    clip_id: ClipId,
    start_beat: Beats,
    duration_beats: Beats,
    in_point: Seconds,
    is_generator: bool,
    is_looping: bool,
}

// ── InteractionOverlay ──────────────────────────────────────────
// Unity InteractionOverlay lines 17-73.

pub struct InteractionOverlay {
    // Dependencies
    clip_vertical_padding: f32,

    // Drag state (Unity lines 37-73) — EXCLUSIVELY owned here
    drag_mode: DragMode,
    drag_anchor_clip_id: Option<ClipId>,
    drag_start_layer_index: usize,
    drag_snapshots: Vec<DragSnapshot>,
    drag_snapshot_clip_ids: HashSet<ClipId>,
    drag_selection_min_start_beat: Beats,
    drag_selection_min_layer: usize,
    drag_selection_max_layer: usize,
    trim_clip_id: Option<ClipId>,
    drag_layer_blocked: bool,
    // Alt/Opt held at move-drag start → on release, leave a copy of each moved
    // clip at its original position (opt-drag duplicate).
    duplicate_on_release: bool,
    region_drag_start_beat: Beats,
    region_drag_start_layer: usize,

    // Move-drag anchor geometry, captured at grab time. (Formerly mirrored on
    // UIState — folded here so transient gesture state has one owner.)
    drag_start_beat: Beats,
    drag_offset_beats: Beats, // offset from the anchor clip's start to the mouse beat

    // Trim originals for the GRABBED clip, captured at grab time — these drive
    // the snap context and the shared edge delta.
    trim_original_start_beat: Beats,
    trim_original_duration_beats: Beats,
    trim_original_in_point: Seconds, // video source offset
    // Pre-trim geometry for EVERY selected clip — the trim delta fans over
    // these, and each changed clip is recorded into the undo batch on drag end.
    trim_originals: Vec<TrimOriginal>,

    // Current modifier state — set by app before each event.
    // Unity reads Keyboard.current inline; Rust stores latest modifiers here.
    modifiers: Modifiers,

    // P4 Unit A automation-point drag state (`docs/AUTOMATION_LANES_DESIGN.md`
    // §7) — `Some` only while `drag_mode == DragMode::AutomationPoint`.
    automation_drag: Option<AutomationDragState>,
    // P4 Unit B segment-gesture state — `Some` only while `drag_mode` is the
    // matching `AutomationSegmentBend`/`AutomationSegmentDrag` variant.
    automation_segment_bend: Option<AutomationSegmentBendState>,
    automation_segment_drag: Option<AutomationSegmentDragState>,
    // P4 Unit B marquee / group-move / draw-mode state — `Some` only while
    // `drag_mode` is the matching variant.
    automation_marquee: Option<AutomationMarqueeState>,
    automation_group_drag: Option<AutomationGroupDragState>,
    automation_draw: Option<AutomationDrawState>,

    // ── P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D15/D17) ──────────────
    // Purely visual drag feedback — the model already moves/snaps instantly
    // (D15: data first, visual follows); these only change what the render
    // loop draws for the dragged clip set. Shared scalars, not per-clip:
    // every clip in a move-drag starts/ends its lift, ghost, and settle at
    // the SAME moment (drag begin / `finalize_move_snap`), so one `AnimF32`
    // per effect — applied uniformly to every id in `drag_visual_clip_ids`
    // — is correct, not an approximation.
    /// Grab lift (D17): targets 1.0 while `drag_mode == Move`, 0.0
    /// otherwise. The render loop reads [`Self::lift_amount`] for a 1-2px
    /// rise on every dragged clip (the shadow itself is unconditional on
    /// `selected` already — see `clip_draw::emit_clip_shadows` — so lift
    /// only adds the rise + lets the caller boost that shadow's opacity).
    lift_anim: AnimF32,
    /// Duplicate-drag ghost (D17): targets 0.5 while alt-dragging
    /// (`drag_mode == Move && duplicate_on_release`), 1.0 otherwise — eases
    /// back up ("solidifies") once the drag ends and the real duplicate
    /// commits in `on_end_drag`.
    ghost_alpha: AnimF32,
    /// Grid-settle offset in PIXELS (D15): seeded in `finalize_move_snap`
    /// with the just-applied snap correction (screen-space) and eased to
    /// 0 — the model is already at its final snapped position; this only
    /// eases the DRAWN rect there from where the release-frame visual sat.
    settle_dx: AnimF32,
    /// The clip ids [`Self::lift_anim`]/[`Self::ghost_alpha`]/
    /// [`Self::settle_dx`] apply to. During a live move-drag this mirrors
    /// `drag_snapshot_clip_ids`; `on_end_drag` snapshots it here BEFORE
    /// clearing the real drag state, so the tweens have something to keep
    /// easing against post-release — the same "keep drawing past the state
    /// event" idea the exit-state pattern uses for deletion, applied here to
    /// a visual settle instead. Cleared once every tween above has settled
    /// (see `tick`).
    drag_visual_clip_ids: Vec<ClipId>,
    /// Landing-line flash (D15): fires once in `finalize_move_snap` when the
    /// snap correction actually moved something. Geometry for the render
    /// loop's vertical line: the snapped beat + the dragged selection's
    /// layer span.
    landing_flash: Transient,
    landing_flash_beat: Beats,
    landing_flash_layers: (usize, usize),
    /// Error shake (D17): fires once on the RISING edge of
    /// `drag_layer_blocked` (a cross-layer move rejected mid-drag) — a
    /// 3px/240ms horizontal shake applied to every dragged clip.
    /// `was_layer_blocked` detects the edge so a held-blocked drag doesn't
    /// re-fire every frame.
    error_shake: Transient,
    was_layer_blocked: bool,
}

impl InteractionOverlay {
    pub fn new(clip_vertical_padding: f32) -> Self {
        Self {
            clip_vertical_padding,
            drag_mode: DragMode::None,
            drag_anchor_clip_id: None,
            drag_start_layer_index: 0,
            drag_snapshots: Vec::with_capacity(8),
            drag_snapshot_clip_ids: HashSet::with_capacity(8),
            drag_selection_min_start_beat: Beats::ZERO,
            drag_selection_min_layer: 0,
            drag_selection_max_layer: 0,
            trim_clip_id: None,
            drag_layer_blocked: false,
            duplicate_on_release: false,
            region_drag_start_beat: Beats::ZERO,
            region_drag_start_layer: 0,
            drag_start_beat: Beats::ZERO,
            drag_offset_beats: Beats::ZERO,
            trim_original_start_beat: Beats::ZERO,
            trim_original_duration_beats: Beats::ZERO,
            trim_original_in_point: Seconds::ZERO,
            trim_originals: Vec::with_capacity(8),
            modifiers: Modifiers::NONE,
            automation_drag: None,
            automation_segment_bend: None,
            automation_segment_drag: None,
            automation_marquee: None,
            automation_group_drag: None,
            automation_draw: None,
            lift_anim: AnimF32::new(0.0, color::MOTION_MED_MS),
            ghost_alpha: AnimF32::new(1.0, color::MOTION_MED_MS),
            settle_dx: AnimF32::new(0.0, color::MOTION_MED_MS),
            drag_visual_clip_ids: Vec::with_capacity(8),
            landing_flash: Transient::default(),
            landing_flash_beat: Beats::ZERO,
            landing_flash_layers: (0, 0),
            error_shake: Transient::default(),
            was_layer_blocked: false,
        }
    }

    // ── P2 motion (`UI_CRAFT_AND_MOTION_PLAN.md` D15/D17) ──────────────

    /// Per-frame tween tick for the overlay's drag-visual pieces (grab lift,
    /// duplicate ghost, grid settle, landing-line flash, error shake). Call
    /// once per frame from the app's frame loop — mirrors
    /// `GraphCanvas::tick`. Returns `true` while anything is still
    /// animating, so the caller can keep the timeline dirty/repainting.
    pub fn tick(&mut self, dt_ms: f32) -> bool {
        let mut any = false;

        let dragging_move = self.drag_mode == DragMode::Move;
        if dragging_move {
            self.drag_visual_clip_ids.clear();
            self.drag_visual_clip_ids
                .extend(self.drag_snapshot_clip_ids.iter().cloned());
        }

        self.lift_anim.set_target(if dragging_move { 1.0 } else { 0.0 });
        any |= self.lift_anim.tick(dt_ms);

        self.ghost_alpha
            .set_target(if dragging_move && self.duplicate_on_release { 0.5 } else { 1.0 });
        any |= self.ghost_alpha.tick(dt_ms);

        any |= self.settle_dx.tick(dt_ms);
        any |= self.landing_flash.tick(dt_ms);
        any |= self.error_shake.tick(dt_ms);

        // Drop the settling clip-id memory once every visual has caught up
        // and no new drag is in flight — otherwise a rapid re-drag right
        // after release would still see stale ids from the previous
        // gesture (harmless — they'd just get overwritten above — but
        // there's no reason to hold them once nothing reads them).
        if !dragging_move
            && !self.lift_anim.is_animating()
            && !self.ghost_alpha.is_animating()
            && !self.settle_dx.is_animating()
        {
            self.drag_visual_clip_ids.clear();
        }
        any
    }

    /// Whether `clip_id` is currently a target of the drag-visual tweens
    /// (grab lift / duplicate ghost / grid settle) — either live-dragged
    /// right now, or still easing out post-release. The render loop checks
    /// this per visible clip before applying [`Self::lift_amount`] /
    /// [`Self::ghost_alpha`] / [`Self::settle_dx_px`].
    pub fn is_drag_visual_target(&self, clip_id: &ClipId) -> bool {
        self.drag_visual_clip_ids.contains(clip_id)
    }

    /// 0..1 grab-lift progress — the render loop derives a 1-2px rise from
    /// this for every [`Self::is_drag_visual_target`] clip.
    pub fn lift_amount(&self) -> f32 {
        self.lift_anim.value().clamp(0.0, 1.0)
    }

    /// 0..1 alpha for a duplicate-drag ghost (1.0 = fully solid — the
    /// common case outside an alt-drag, so callers can multiply
    /// unconditionally).
    pub fn ghost_alpha(&self) -> f32 {
        self.ghost_alpha.value().clamp(0.0, 1.0)
    }

    /// Current grid-settle X offset in screen pixels (D15) — added to the
    /// dragged clip's already-final (snapped) drawn rect, decaying to 0.
    pub fn settle_dx_px(&self) -> f32 {
        self.settle_dx.value()
    }

    /// Current error-shake X offset in screen pixels (D17) — a decaying
    /// sine over the shake's 240ms, `None` when idle.
    pub fn error_shake_offset_px(&self) -> f32 {
        match self.error_shake.progress() {
            Some(p) => {
                let decay = 1.0 - p;
                (p * std::f32::consts::PI * 5.0).sin() * 3.0 * decay
            }
            None => 0.0,
        }
    }

    /// `Some(progress)` while the landing-line flash (D15) is active, plus
    /// its screen geometry: the snapped beat and the dragged selection's
    /// `(min_layer, max_layer)` span.
    pub fn landing_flash(&self) -> Option<(f32, Beats, usize, usize)> {
        self.landing_flash
            .progress()
            .map(|p| (p, self.landing_flash_beat, self.landing_flash_layers.0, self.landing_flash_layers.1))
    }

    /// True while any drag is in progress. Unity: IsDragging property.
    pub fn is_dragging(&self) -> bool {
        self.drag_mode != DragMode::None
    }

    /// Current drag mode (read-only, for external queries like auto-scroll).
    pub fn drag_mode(&self) -> DragMode {
        self.drag_mode
    }

    /// B13 — the clip whose position/length the live readout should report,
    /// or `None` outside a move/trim gesture. Rubber-band (`RegionSelect`)
    /// has no single clip to report and is deliberately `None` here —
    /// `ClipId` wraps `Arc<str>`, so this clone is a refcount bump, not an
    /// allocation.
    pub fn drag_readout_clip_id(&self) -> Option<ClipId> {
        match self.drag_mode {
            DragMode::Move => self.drag_anchor_clip_id.clone(),
            DragMode::TrimLeft | DragMode::TrimRight => self.trim_clip_id.clone(),
            _ => None,
        }
    }

    /// Update the stored modifier state. Call from app before dispatching events.
    /// Unity reads Keyboard.current inline; Rust stores the latest state here.
    pub fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
    }

    /// Capture the anchor clip's start beat and the grab offset for a move drag.
    /// The single place move-drag geometry is recorded (formerly `UIState::begin_drag`).
    fn begin_move(&mut self, anchor_start_beat: Beats, mouse_beat: Beats) {
        self.drag_start_beat = anchor_start_beat;
        self.drag_offset_beats = mouse_beat - anchor_start_beat;
    }

    /// Capture a clip's pre-trim geometry for the undo command (formerly
    /// `UIState::begin_trim_left` / `begin_trim_right` — the left/right flag was
    /// unused; the drag mode already distinguishes the edge).
    fn begin_trim(&mut self, clip: &ClipRef) {
        self.trim_original_start_beat = clip.start_beat;
        self.trim_original_duration_beats = clip.duration_beats;
        self.trim_original_in_point = clip.in_point;
    }

    /// Capture pre-trim geometry for every selected clip, so a trim drag fans
    /// the grabbed clip's edge delta over the whole selection and records one
    /// batched undo entry. Locked clips are skipped. The `on_begin_drag` select
    /// guard ensures the grabbed clip is in the selection before this runs.
    fn capture_trim_selection(&mut self, ui_state: &UIState, host: &dyn TimelineEditingHost) {
        self.trim_originals.clear();
        for id in ui_state.get_selected_clip_ids() {
            if let Some(clip) = host.find_clip_by_id(&id) {
                if clip.is_locked {
                    continue;
                }
                self.trim_originals.push(TrimOriginal {
                    clip_id: id.clone(),
                    start_beat: clip.start_beat,
                    duration_beats: clip.duration_beats,
                    in_point: clip.in_point,
                    is_generator: clip.is_generator,
                    is_looping: clip.is_looping,
                });
            }
        }
    }

    // ────────────────────────────────────────────────────────────
    // DRAG POLLING
    // Unity InteractionOverlay.PollMoveDrag (lines 116-124), extended (B11) to
    // trim and region-select — edge autoscroll during move/trim/rubber-band
    // must keep advancing when the pointer is parked at the edge, not just
    // when a mouse-move event arrives. Called from app.rs frame loop every
    // frame while a drag of one of these kinds is in flight.
    // ────────────────────────────────────────────────────────────

    pub fn poll_drag(
        &mut self,
        mouse_screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
    ) {
        match self.drag_mode {
            DragMode::Move if self.drag_anchor_clip_id.is_some() => {
                self.handle_move_drag(mouse_screen_pos, host, ui_state, viewport);
            }
            DragMode::TrimLeft => {
                self.handle_trim_left_drag(mouse_screen_pos, host, viewport);
            }
            DragMode::TrimRight => {
                self.handle_trim_right_drag(mouse_screen_pos, host, viewport);
            }
            DragMode::RegionSelect => {
                self.update_region_drag(mouse_screen_pos, ui_state, viewport, host);
            }
            _ => {}
        }
    }

    // ────────────────────────────────────────────────────────────
    // POINTER EVENTS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.OnPointerClick (lines 130-217).
    pub fn on_pointer_click(
        &mut self,
        pos: Vec2,
        shift: bool,
        ctrl: bool,
        click_count: u32,
        is_right_button: bool,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        // P4 Unit A (`docs/AUTOMATION_LANES_DESIGN.md` §7): a click inside an
        // automation lane strip is handled entirely here — click-on-line adds
        // a breakpoint, click-on-dot selects it, double-click-on-dot deletes
        // it — and never falls through to clip/region logic below. Right-
        // clicks are left alone (no automation context menu in Unit A).
        if !is_right_button
            && self.handle_automation_click(pos, click_count, host, ui_state, viewport)
        {
            return;
        }

        let hit = self.hit_test_at(pos, viewport);

        if hit.is_none() {
            // ── NO HIT: empty area clicked ──
            // Unity line 147: clear region
            ui_state.clear_region();

            let layer_index = viewport.layer_at_y(pos.y);

            // Unity: InputHandler.HandleEmptyAreaRightClick → ShowLayerContextMenu
            if is_right_button {
                if let Some(layer) = layer_index {
                    let beat = viewport.pixel_to_beat(pos.x);
                    host.on_track_right_click(beat, layer, pos);
                }
                return;
            }

            // Unity lines 152-162: double-click on empty area → create clip
            if click_count >= 2
                && let Some(layer) = layer_index
            {
                let beat = viewport.floor_to_grid(viewport.pixel_to_beat(pos.x));
                if let Some(clip_id) =
                    host.create_clip_at_position(beat, layer, viewport.clip_creation_step())
                {
                    let lid = host.layer_id_at_index(layer).unwrap_or_default();
                    ui_state.select_clip(clip_id.clone(), lid);
                    host.on_clip_selected(&clip_id);
                }
                return;
            }

            // Unity lines 165-188: single click on empty area
            if let Some(layer) = layer_index {
                let beat = viewport.pixel_to_beat(pos.x);
                let snapped = viewport.snap_to_grid(beat);

                if shift {
                    // Unity line 180: Shift+Click → extend region
                    select_region_to(snapped, layer, ui_state, host);
                } else {
                    // Unity line 184: bare click → set insert cursor
                    let lid = host.layer_id_at_index(layer).unwrap_or_default();
                    ui_state.set_insert_cursor(snapped, lid);
                }

                // Unity line 187: always inspect layer on empty click
                host.inspect_layer(layer);
            }
            return;
        }

        let hit = hit.unwrap();

        // Unity line 195: locked clips — ignore
        if self.clip_is_locked(&hit.clip_id, viewport) {
            return;
        }

        // Unity lines 198-204: right-click → context menu
        if is_right_button {
            if !ui_state.is_selected(&hit.clip_id) {
                let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
                ui_state.select_clip(hit.clip_id.clone(), lid);
            }
            host.on_clip_right_click(&hit.clip_id, pos);
            return;
        }

        // Unity lines 206-214: selection modifiers
        if shift {
            // D2: shift-click on a CLIP is a clip-range selection (contiguous
            // whole clips on the anchor's layer), not a region — the empty-area
            // shift path above still calls `select_region_to`; only this
            // clip-hit arm changes (S1/S3's root).
            select_clip_range_to(&hit.clip_id, ui_state, host);
        } else if ctrl {
            // Unity lines 208-212: Ctrl → toggle multi-select. D1: no longer
            // synthesises a region from the clip set — a multi-clip selection
            // is a pure `Clips` selection (the redundant region band is gone;
            // begins the S1 fix). `toggle_clip_selection` owns the whole update.
            let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
            ui_state.toggle_clip_selection(hit.clip_id.clone(), lid);
        } else {
            // Unity line 214: bare click → select single
            let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
            ui_state.select_clip(hit.clip_id.clone(), lid);
        }

        // Unity line 216: always notify host
        host.on_clip_selected(&hit.clip_id);
    }

    // ────────────────────────────────────────────────────────────
    // AUTOMATION LANE EDITING (P4 Unit A, `docs/AUTOMATION_LANES_DESIGN.md` §7)
    // ────────────────────────────────────────────────────────────

    /// Handle a click/double-click that lands inside an automation lane
    /// strip. Returns `true` when the click was handled (caller must
    /// return without falling through to clip/region logic).
    ///
    /// - Click on an existing dot → select it (Delete key removes it later).
    /// - Double-click on an existing dot → remove it immediately.
    /// - Click (or double-click) on bare strip → add a breakpoint at the
    ///   clicked beat/value (grid-snapped unless Cmd is held; `Hold` shape
    ///   for whole-numbers params, `Linear` otherwise — §8).
    fn handle_automation_click(
        &mut self,
        pos: Vec2,
        click_count: u32,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) -> bool {
        let lanes = viewport.automation_lane_screens(&[]);
        let Some(hit) = automation_hit_tester::hit_test_automation(pos, &lanes) else {
            return false;
        };
        match hit {
            AutomationHit::Dot { lane_index, dot_index } => {
                let lane = &lanes[lane_index];
                let dot = lane.dots[dot_index];
                if click_count >= 2 {
                    host.remove_automation_point(&lane.target, &lane.param_id, dot.beat);
                    if ui_state.selected_automation_point.as_ref().is_some_and(|s| {
                        s.target == lane.target && s.param_id == lane.param_id && s.beat.0 == dot.beat.0
                    }) {
                        ui_state.selected_automation_point = None;
                    }
                } else {
                    ui_state.selected_automation_point = Some(UiAutomationPointRef {
                        target: lane.target.clone(),
                        param_id: lane.param_id.clone(),
                        beat: dot.beat,
                    });
                }
            }
            // A plain CLICK (no drag) on a segment inserts a new breakpoint
            // there, same as clicking bare strip — Ableton's "click the line
            // to add a point" behavior. `Segment` only changes DRAG-begin
            // routing (`begin_automation_drag`), not click routing.
            AutomationHit::Strip { lane_index } | AutomationHit::Segment { lane_index, .. } => {
                let lane = &lanes[lane_index];
                let raw_beat = viewport.pixel_to_beat(pos.x);
                let beat = if self.modifiers.command {
                    raw_beat
                } else {
                    viewport.snap_to_grid(raw_beat)
                }
                .max(Beats::ZERO);
                let norm = (1.0
                    - (pos.y - lane.strip_rect.y) / lane.strip_rect.height.max(f32::EPSILON))
                .clamp(0.0, 1.0);
                let value = lane.param_min + norm * (lane.param_max - lane.param_min);
                let shape = if lane.whole_numbers {
                    UiSegmentShape::Hold
                } else {
                    UiSegmentShape::Linear
                };
                host.add_automation_point(&lane.target, &lane.param_id, beat, value, shape);
            }
        }
        true
    }

    /// Hit-test `press_pos` against automation lane strips for a drag begin.
    /// Returns `true` when an existing dot was grabbed (caller must return
    /// without falling through to clip drag logic). A drag press on bare
    /// strip area is NOT handled here — §7's "click on line adds a point" is
    /// a click action (`handle_automation_click`), not a drag-begin; the
    /// `DragBegin` event only fires for what the platform layer already
    /// distinguishes as a drag gesture, so a plain click on the strip is
    /// routed through `on_pointer_click` instead.
    fn begin_automation_drag(
        &mut self,
        press_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) -> bool {
        let lanes = viewport.automation_lane_screens(&[]);
        let hit = automation_hit_tester::hit_test_automation(press_pos, &lanes);
        let Some(hit) = hit else {
            return false;
        };

        // Pencil/draw mode overrides ALL other drag routing while active —
        // Ableton's pencil draws regardless of whether the press happened to
        // land on an existing dot/segment (P4 Unit B, §7's "Draw mode").
        if ui_state.automation_draw_mode {
            let lane_index = match hit {
                AutomationHit::Dot { lane_index, .. }
                | AutomationHit::Segment { lane_index, .. }
                | AutomationHit::Strip { lane_index } => lane_index,
            };
            self.begin_automation_draw(lane_index, press_pos, &lanes, host, viewport);
            return true;
        }

        match hit {
            AutomationHit::Dot { lane_index, dot_index } => {
                let lane = &lanes[lane_index];
                let dot = lane.dots[dot_index];
                let point_ref = UiAutomationPointRef {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    beat: dot.beat,
                };
                // Grabbing a dot that's part of an active multi-selection (2+
                // members) moves the WHOLE group; otherwise this is a plain
                // single-point drag and any stale marquee selection is
                // cleared (mirrors clip selection's "bare click selects just
                // this one").
                if ui_state.selected_automation_points.len() > 1
                    && ui_state.selected_automation_points.contains(&point_ref)
                {
                    self.begin_automation_group_drag(press_pos, &lanes, ui_state, host);
                    return true;
                }
                ui_state.selected_automation_points.clear();

                let value =
                    lane.param_min + dot.value_norm.clamp(0.0, 1.0) * (lane.param_max - lane.param_min);
                self.automation_drag = Some(AutomationDragState {
                    target: lane.target.clone(),
                    param_id: lane.param_id.clone(),
                    original: (dot.beat, value, dot.shape),
                    last_beat: dot.beat,
                    last_value: value,
                });
                ui_state.selected_automation_point = Some(point_ref);
                self.drag_mode = DragMode::AutomationPoint;
                host.set_cursor(TimelineCursor::Move);
                true
            }
            AutomationHit::Segment { lane_index, left_dot_index } => {
                self.begin_automation_segment_drag(lane_index, left_dot_index, press_pos, &lanes, host);
                true
            }
            AutomationHit::Strip { .. } => {
                self.automation_marquee = Some(AutomationMarqueeState { start: press_pos });
                ui_state.selected_automation_points.clear();
                self.drag_mode = DragMode::AutomationMarquee;
                true
            }
        }
    }

    /// Begin a marquee-group drag: capture every currently multi-selected
    /// point's original (beat, value, shape) plus its own lane's param
    /// range — points can span multiple lanes/params at once, so only the
    /// NORMALIZED delta is shared, not the raw param-range delta.
    fn begin_automation_group_drag(
        &mut self,
        press_pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        ui_state: &UIState,
        host: &mut dyn TimelineEditingHost,
    ) {
        let Some(AutomationHit::Dot { lane_index: grab_lane_index, .. }) =
            automation_hit_tester::hit_test_automation(press_pos, lanes)
        else {
            return;
        };
        let grab_lane = &lanes[grab_lane_index];
        let grab_norm = (1.0
            - (press_pos.y - grab_lane.strip_rect.y) / grab_lane.strip_rect.height.max(f32::EPSILON))
        .clamp(0.0, 1.0);

        let mut points = Vec::with_capacity(ui_state.selected_automation_points.len());
        for r in &ui_state.selected_automation_points {
            let Some(lane) = lanes.iter().find(|l| l.target == r.target && l.param_id == r.param_id) else {
                continue;
            };
            let Some(dot) = lane.dots.iter().find(|d| d.beat.0 == r.beat.0) else {
                continue;
            };
            let range = lane.param_max - lane.param_min;
            let value = lane.param_min + dot.value_norm.clamp(0.0, 1.0) * range;
            points.push(AutomationGroupPointState {
                target: lane.target.clone(),
                param_id: lane.param_id.clone(),
                beat: dot.beat,
                original_value: value,
                shape: dot.shape,
                param_min: lane.param_min,
                param_max: lane.param_max,
                last_value: value,
            });
        }
        if points.is_empty() {
            return;
        }
        self.automation_group_drag = Some(AutomationGroupDragState {
            grab_target: grab_lane.target.clone(),
            grab_param_id: grab_lane.param_id.clone(),
            grab_norm,
            points,
        });
        self.drag_mode = DragMode::AutomationGroupMove;
        host.set_cursor(TimelineCursor::Move);
    }

    /// Begin a pencil/draw stroke on `lane_index`'s lane: reads the FULL
    /// pre-stroke point list via `host.automation_lane_points` (NOT
    /// `AutomationLaneScreen::dots`, which is culled to the visible beat
    /// range — using it here would silently drop off-screen points from the
    /// eventual install), seeds `working` from it, then writes the first
    /// grid step under the press.
    fn begin_automation_draw(
        &mut self,
        lane_index: usize,
        press_pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let lane = &lanes[lane_index];
        let old_points = host.automation_lane_points(&lane.target, &lane.param_id);
        let working = old_points.clone().unwrap_or_default();
        self.automation_draw = Some(AutomationDrawState {
            target: lane.target.clone(),
            param_id: lane.param_id.clone(),
            new_point_shape: if lane.whole_numbers { UiSegmentShape::Hold } else { UiSegmentShape::Linear },
            old_points,
            working,
        });
        self.drag_mode = DragMode::AutomationDraw;
        host.set_cursor(TimelineCursor::Move);
        // Falls through to the shared per-frame writer so the press itself
        // draws a point immediately, not just subsequent movement.
        self.write_automation_draw_step(press_pos, host, viewport);
    }

    /// Write (insert-or-overwrite) the grid-snapped point under `pos` into
    /// the in-progress draw stroke, then push the whole working list to the
    /// live preview. Re-resolves the stroke's lane fresh each call (handles
    /// mid-stroke scroll), matching every other drag handler's discipline.
    fn write_automation_draw_step(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(state) = self.automation_draw.as_ref() else {
            return;
        };
        let (target, param_id) = (state.target.clone(), state.param_id.clone());
        let lanes = viewport.automation_lane_screens(&[]);
        let Some(lane) = lanes.iter().find(|l| l.target == target && l.param_id == param_id) else {
            return;
        };

        let beat = viewport.snap_to_grid(viewport.pixel_to_beat(pos.x)).max(Beats::ZERO);
        let norm = (1.0 - (pos.y - lane.strip_rect.y) / lane.strip_rect.height.max(f32::EPSILON)).clamp(0.0, 1.0);
        let value = lane.param_min + norm * (lane.param_max - lane.param_min);

        let Some(state) = self.automation_draw.as_mut() else {
            return;
        };
        apply_draw_point(&mut state.working, beat, value, state.new_point_shape);
        let snapshot = state.working.clone();
        host.set_automation_draw_preview(&target, &param_id, snapshot);
    }

    /// Commit a finished draw stroke as one undo entry — no-op if the
    /// working list ended up identical to the pre-stroke set.
    fn commit_automation_draw(&mut self, host: &mut dyn TimelineEditingHost) {
        let Some(state) = self.automation_draw.take() else {
            return;
        };
        let unchanged = state.old_points.as_deref() == Some(state.working.as_slice())
            || (state.old_points.is_none() && state.working.is_empty());
        if !unchanged {
            host.commit_automation_draw_stroke(&state.target, &state.param_id, state.working, state.old_points);
        }
    }

    /// Grab a segment for either an Alt-drag curve bend or a plain vertical
    /// drag (P4 Unit B, §7). Curve-bend is gated off for `whole_numbers`
    /// lanes (§8: enum/int params author with `Hold`, so bending one would
    /// silently change its runtime sampling from a step to a curve) — Alt on
    /// such a lane falls back to the vertical-drag gesture instead of a no-op.
    fn begin_automation_segment_drag(
        &mut self,
        lane_index: usize,
        left_dot_index: usize,
        press_pos: Vec2,
        lanes: &[crate::panels::viewport::AutomationLaneScreen],
        host: &mut dyn TimelineEditingHost,
    ) {
        let lane = &lanes[lane_index];
        let left = lane.dots[left_dot_index];
        let right = lane.dots[left_dot_index + 1];
        let range = lane.param_max - lane.param_min;
        let left_value = lane.param_min + left.value_norm.clamp(0.0, 1.0) * range;
        let right_value = lane.param_min + right.value_norm.clamp(0.0, 1.0) * range;

        if self.modifiers.alt && !lane.whole_numbers {
            let original_bend = match left.shape {
                UiSegmentShape::Curved(c) => c,
                _ => 0.0,
            };
            self.automation_segment_bend = Some(AutomationSegmentBendState {
                target: lane.target.clone(),
                param_id: lane.param_id.clone(),
                left_beat: left.beat,
                left_value,
                original_shape: left.shape,
                grab_y: press_pos.y,
                last_bend: original_bend,
            });
            self.drag_mode = DragMode::AutomationSegmentBend;
        } else {
            let grab_norm = (1.0 - (press_pos.y - lane.strip_rect.y) / lane.strip_rect.height.max(f32::EPSILON))
                .clamp(0.0, 1.0);
            self.automation_segment_drag = Some(AutomationSegmentDragState {
                target: lane.target.clone(),
                param_id: lane.param_id.clone(),
                left_beat: left.beat,
                left_original_value: left_value,
                left_shape: left.shape,
                right_beat: right.beat,
                right_original_value: right_value,
                right_shape: right.shape,
                grab_norm,
                last_left_value: left_value,
                last_right_value: right_value,
            });
            self.drag_mode = DragMode::AutomationSegmentDrag;
        }
        host.set_cursor(TimelineCursor::Move);
    }

    /// Live-preview an in-progress automation point drag (`DragMode::
    /// AutomationPoint`). Re-derives beat/value fresh from the current
    /// screen geometry each frame (not incrementally) — mirrors
    /// `handle_move_drag`'s "recompute from origin" discipline, and stays
    /// correct if the viewport scrolls vertically mid-drag (the strip's Y
    /// re-resolves every call, unlike a cached `strip_rect`).
    fn handle_automation_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some((target, param_id, from_beat)) = self
            .automation_drag
            .as_ref()
            .map(|d| (d.target.clone(), d.param_id.clone(), d.last_beat))
        else {
            return;
        };
        let lanes = viewport.automation_lane_screens(&[]);
        let Some(lane) = lanes
            .iter()
            .find(|l| l.target == target && l.param_id == param_id)
        else {
            return;
        };

        let raw_beat = viewport.pixel_to_beat(pos.x);
        let to_beat = if self.modifiers.command {
            raw_beat
        } else {
            viewport.snap_to_grid(raw_beat)
        }
        .max(Beats::ZERO);
        let norm = (1.0
            - (pos.y - lane.strip_rect.y) / lane.strip_rect.height.max(f32::EPSILON))
        .clamp(0.0, 1.0);
        let to_value = lane.param_min + norm * (lane.param_max - lane.param_min);

        host.set_automation_point_preview(&target, &param_id, from_beat, to_beat, to_value);

        if let Some(drag) = self.automation_drag.as_mut() {
            drag.last_beat = to_beat;
            drag.last_value = to_value;
        }
    }

    /// Commit a finished automation point drag as one undo entry. No-op
    /// (still clears state) if the point never actually moved.
    fn commit_automation_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        let Some(drag) = self.automation_drag.take() else {
            return;
        };
        let new_point = (drag.last_beat, drag.last_value, drag.original.2);
        let moved = (new_point.0.0 - drag.original.0.0).abs() >= 0.0001
            || (new_point.1 - drag.original.1).abs() > f32::EPSILON;
        if moved {
            host.commit_automation_point_move(&drag.target, &drag.param_id, drag.original, new_point);
        }
    }

    /// Pixel range of vertical drag mapped to the full `-1..1` bend swing —
    /// an interior tuning constant (Alt-drag curve bend, P4 Unit B).
    const SEGMENT_BEND_PX_RANGE: f32 = 80.0;

    /// Live-preview an in-progress Alt-drag curve bend. Re-derives `bend`
    /// fresh from the vertical delta since grab each frame (never
    /// incrementally), same discipline as every other drag handler here.
    /// Dragging UP (screen Y decreasing) bends positive.
    fn handle_automation_segment_bend_drag(&mut self, pos: Vec2, host: &mut dyn TimelineEditingHost) {
        let Some(state) = self.automation_segment_bend.as_ref() else {
            return;
        };
        let mut delta_px = state.grab_y - pos.y;
        if self.modifiers.shift {
            delta_px *= 0.25; // fine adjustment, mirrors §7's Shift-drag convention
        }
        let bend = (delta_px / Self::SEGMENT_BEND_PX_RANGE).clamp(-1.0, 1.0);
        host.set_automation_segment_bend_preview(&state.target, &state.param_id, state.left_beat, bend);
        if let Some(state) = self.automation_segment_bend.as_mut() {
            state.last_bend = bend;
        }
    }

    /// Commit a finished curve-bend drag as one undo entry — reuses
    /// `commit_automation_point_move` directly: beat and value are
    /// untouched by this gesture, only `shape` differs between old and new.
    fn commit_automation_segment_bend(&mut self, host: &mut dyn TimelineEditingHost) {
        let Some(state) = self.automation_segment_bend.take() else {
            return;
        };
        let new_shape = UiSegmentShape::Curved(state.last_bend);
        if new_shape != state.original_shape {
            let old = (state.left_beat, state.left_value, state.original_shape);
            let new = (state.left_beat, state.left_value, new_shape);
            host.commit_automation_point_move(&state.target, &state.param_id, old, new);
        }
    }

    /// Live-preview an in-progress vertical segment drag. Re-derives both
    /// endpoints' values fresh from the normalized delta since grab each
    /// frame — the delta is computed once (not per-point), then each
    /// endpoint clamps independently to its own `0..1` range, matching how a
    /// multi-clip drag clamps each clip independently at the timeline edge.
    fn handle_automation_segment_vertical_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some(state) = self.automation_segment_drag.clone() else {
            return;
        };
        let lanes = viewport.automation_lane_screens(&[]);
        let Some(lane) = lanes
            .iter()
            .find(|l| l.target == state.target && l.param_id == state.param_id)
        else {
            return;
        };
        let norm = (1.0 - (pos.y - lane.strip_rect.y) / lane.strip_rect.height.max(f32::EPSILON))
            .clamp(0.0, 1.0);
        let mut delta_norm = norm - state.grab_norm;
        if self.modifiers.shift {
            delta_norm *= 0.25;
        }
        let range = (lane.param_max - lane.param_min).max(f32::EPSILON);
        let left_norm =
            ((state.left_original_value - lane.param_min) / range + delta_norm).clamp(0.0, 1.0);
        let right_norm =
            ((state.right_original_value - lane.param_min) / range + delta_norm).clamp(0.0, 1.0);
        let left_value = lane.param_min + left_norm * range;
        let right_value = lane.param_min + right_norm * range;

        host.set_automation_segment_drag_preview(
            &state.target,
            &state.param_id,
            state.left_beat,
            left_value,
            state.right_beat,
            right_value,
        );
        if let Some(s) = self.automation_segment_drag.as_mut() {
            s.last_left_value = left_value;
            s.last_right_value = right_value;
        }
    }

    /// Commit a finished vertical segment drag as ONE undo entry covering
    /// both endpoints (`host.commit_automation_segment_drag` batches them via
    /// `ContentCommand::ExecuteBatch`/`CompositeCommand`).
    fn commit_automation_segment_value_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        let Some(state) = self.automation_segment_drag.take() else {
            return;
        };
        let moved = (state.last_left_value - state.left_original_value).abs() > f32::EPSILON
            || (state.last_right_value - state.right_original_value).abs() > f32::EPSILON;
        if moved {
            host.commit_automation_segment_drag(
                &state.target,
                &state.param_id,
                (state.left_beat, state.left_original_value, state.last_left_value, state.left_shape),
                (
                    state.right_beat,
                    state.right_original_value,
                    state.last_right_value,
                    state.right_shape,
                ),
            );
        }
    }

    /// Live-update the marquee selection every frame: rebuild the rect from
    /// the press corner to the CURRENT position, then re-select every dot
    /// inside it fresh (never incrementally — the same discipline as every
    /// other drag handler, and it's cheap: typical scale is tens of lanes).
    fn handle_automation_marquee_drag(&mut self, pos: Vec2, ui_state: &mut UIState, viewport: &TimelineViewportPanel) {
        let Some(marquee) = self.automation_marquee else {
            return;
        };
        let rect = automation_hit_tester::marquee_rect(marquee.start, pos);
        let lanes = viewport.automation_lane_screens(&[]);
        let hits = automation_hit_tester::dots_in_rect(rect, &lanes);
        ui_state.selected_automation_points = hits
            .into_iter()
            .map(|(lane_index, dot_index)| {
                let lane = &lanes[lane_index];
                let dot = lane.dots[dot_index];
                UiAutomationPointRef { target: lane.target.clone(), param_id: lane.param_id.clone(), beat: dot.beat }
            })
            .collect();
    }

    /// Live-preview an in-progress marquee GROUP drag. Computes ONE
    /// normalized delta from the grabbed lane's strip (re-resolved fresh
    /// each frame), then applies it to every captured point via the
    /// EXISTING `set_automation_point_preview` (calling it with
    /// `from_beat == to_beat` so only the value changes) — no new preview
    /// plumbing needed.
    fn handle_automation_group_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let Some((grab_target, grab_param_id, grab_norm)) = self
            .automation_group_drag
            .as_ref()
            .map(|s| (s.grab_target.clone(), s.grab_param_id.clone(), s.grab_norm))
        else {
            return;
        };
        let lanes = viewport.automation_lane_screens(&[]);
        let Some(grab_lane) = lanes.iter().find(|l| l.target == grab_target && l.param_id == grab_param_id)
        else {
            return;
        };
        let norm = (1.0
            - (pos.y - grab_lane.strip_rect.y) / grab_lane.strip_rect.height.max(f32::EPSILON))
        .clamp(0.0, 1.0);
        let delta_norm = norm - grab_norm;

        let Some(state) = self.automation_group_drag.as_mut() else {
            return;
        };
        for point in &mut state.points {
            let range = (point.param_max - point.param_min).max(f32::EPSILON);
            let orig_norm = (point.original_value - point.param_min) / range;
            let new_value = point.param_min + (orig_norm + delta_norm).clamp(0.0, 1.0) * range;
            host.set_automation_point_preview(&point.target, &point.param_id, point.beat, point.beat, new_value);
            point.last_value = new_value;
        }
    }

    /// Commit a finished marquee group drag as ONE undo entry covering every
    /// point (`host.commit_automation_group_move` batches them via
    /// `ContentCommand::ExecuteBatch`/`CompositeCommand`). No-op if nothing
    /// actually moved.
    fn commit_automation_group_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        let Some(state) = self.automation_group_drag.take() else {
            return;
        };
        let moves: Vec<_> = state
            .points
            .iter()
            .filter(|p| (p.last_value - p.original_value).abs() > f32::EPSILON)
            .map(|p| (p.target.clone(), p.param_id.clone(), p.beat, p.original_value, p.last_value, p.shape))
            .collect();
        if !moves.is_empty() {
            host.commit_automation_group_move(moves);
        }
    }

    /// Port of Unity InteractionOverlay.OnPointerMove (lines 219-257).
    pub fn on_pointer_move(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        // Unity lines 222-223: track cursor position for paste target
        ui_state.cursor_beat = viewport.pixel_to_beat(pos.x).as_f32();
        ui_state.cursor_layer_id = viewport
            .layer_at_y(pos.y)
            .and_then(|idx| host.layer_id_at_index(idx));

        // Unity lines 225-245: hover detection
        let hit = self.hit_test_at(pos, viewport);
        let new_hover_id = hit.as_ref().map(|h| h.clip_id.clone());

        if new_hover_id != ui_state.hovered_clip_id {
            // Unity lines 230-244: invalidate affected layers on hover change
            if let Some(ref old_id) = ui_state.hovered_clip_id
                && let Some(old_clip) = host.find_clip_by_id(old_id)
            {
                host.invalidate_layer_bitmap(old_clip.layer_index);
            }

            ui_state.hovered_clip_id = new_hover_id;

            if let Some(ref hit) = hit {
                host.invalidate_layer_bitmap(hit.layer_index);
            }
        }

        // Unity lines 248-256: cursor feedback (only when not dragging)
        if self.drag_mode == DragMode::None {
            if let Some(ref hit) = hit {
                match hit.region {
                    HitRegion::TrimLeft | HitRegion::TrimRight => {
                        host.set_cursor(TimelineCursor::ResizeHorizontal);
                    }
                    HitRegion::Body => {
                        host.set_cursor(TimelineCursor::Move);
                    }
                }
            } else {
                host.set_cursor(TimelineCursor::Default);
            }
        }
    }

    /// Port of Unity InteractionOverlay.OnPointerExit (lines 259-272).
    pub fn on_pointer_exit(&mut self, host: &mut dyn TimelineEditingHost, ui_state: &mut UIState) {
        if let Some(ref old_id) = ui_state.hovered_clip_id {
            if let Some(old_clip) = host.find_clip_by_id(old_id) {
                host.invalidate_layer_bitmap(old_clip.layer_index);
            } else {
                host.invalidate_all_layer_bitmaps();
            }
        }
        ui_state.hovered_clip_id = None;
        host.set_cursor(TimelineCursor::Default);
    }

    // ────────────────────────────────────────────────────────────
    // DRAG EVENTS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.OnBeginDrag (lines 278-332).
    /// `press_pos` is the position where the mouse was PRESSED, not current.
    pub fn on_begin_drag(
        &mut self,
        press_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        if crate::input::input_trace_enabled() {
            eprintln!(
                "[input-trace] overlay: begin_drag ({:.0},{:.0}) prior mode={:?}",
                press_pos.x, press_pos.y, self.drag_mode
            );
        }
        self.drag_layer_blocked = false;
        self.was_layer_blocked = false;

        // P4 Unit A: grabbing an existing automation dot starts a point
        // drag instead of clip/region logic — see `begin_automation_drag`'s
        // doc for why bare-strip clicks aren't handled here.
        if self.begin_automation_drag(press_pos, host, ui_state, viewport) {
            return;
        }

        // Unity line 284: hit-test at PRESS position
        let hit = self.hit_test_at(press_pos, viewport);

        if hit.is_none() {
            // Unity lines 288-291: empty area drag → region selection
            self.drag_mode = DragMode::RegionSelect;
            // Unity reads Keyboard.current for ctrl/cmd — we use stored modifiers
            let ctrl = self.modifiers.ctrl || self.modifiers.command;
            self.begin_region_drag(press_pos, ctrl, ui_state, viewport);
            return;
        }

        let hit = hit.unwrap();

        // Unity line 295: locked clips — ignore
        if self.clip_is_locked(&hit.clip_id, viewport) {
            return;
        }

        let beat = viewport.pixel_to_beat(press_pos.x);

        let hit_layer_id = host.layer_id_at_index(hit.layer_index).unwrap_or_default();

        match hit.region {
            // Unity lines 299-309: trim left
            HitRegion::TrimLeft => {
                if !ui_state.is_selected(&hit.clip_id) {
                    ui_state.select_clip(hit.clip_id.clone(), hit_layer_id.clone());
                    host.on_clip_selected(&hit.clip_id);
                }
                self.drag_mode = DragMode::TrimLeft;
                self.trim_clip_id = Some(hit.clip_id.clone());
                if let Some(clip) = host.find_clip_by_id(&hit.clip_id) {
                    self.begin_trim(&clip);
                }
                self.capture_trim_selection(ui_state, host);
            }
            // Unity lines 311-320: trim right
            HitRegion::TrimRight => {
                if !ui_state.is_selected(&hit.clip_id) {
                    ui_state.select_clip(hit.clip_id.clone(), hit_layer_id);
                    host.on_clip_selected(&hit.clip_id);
                }
                self.drag_mode = DragMode::TrimRight;
                self.trim_clip_id = Some(hit.clip_id.clone());
                if let Some(clip) = host.find_clip_by_id(&hit.clip_id) {
                    self.begin_trim(&clip);
                }
                self.capture_trim_selection(ui_state, host);
            }
            // Unity lines 322-324: body → move drag
            HitRegion::Body => {
                // Alt/Opt-drag duplicates: remembered now, acted on at release.
                self.duplicate_on_release = self.modifiers.alt;
                self.begin_move_drag(
                    &hit.clip_id,
                    hit.layer_index,
                    beat,
                    host,
                    ui_state,
                    viewport,
                );
            }
        }

        // Unity lines 328-331: reinforce cursor
        match self.drag_mode {
            DragMode::TrimLeft | DragMode::TrimRight => {
                host.set_cursor(TimelineCursor::ResizeHorizontal);
            }
            DragMode::Move => {
                host.set_cursor(TimelineCursor::Move);
            }
            _ => {}
        }
    }

    /// Port of Unity InteractionOverlay.OnDrag (lines 334-353).
    pub fn on_drag(
        &mut self,
        pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
    ) {
        match self.drag_mode {
            DragMode::Move => {
                self.handle_move_drag(pos, host, ui_state, viewport);
            }
            DragMode::TrimLeft => {
                self.handle_trim_left_drag(pos, host, viewport);
            }
            DragMode::TrimRight => {
                self.handle_trim_right_drag(pos, host, viewport);
            }
            DragMode::RegionSelect => {
                self.update_region_drag(pos, ui_state, viewport, host);
            }
            DragMode::AutomationPoint => {
                self.handle_automation_drag(pos, host, viewport);
            }
            DragMode::AutomationSegmentBend => {
                self.handle_automation_segment_bend_drag(pos, host);
            }
            DragMode::AutomationSegmentDrag => {
                self.handle_automation_segment_vertical_drag(pos, host, viewport);
            }
            DragMode::AutomationMarquee => {
                self.handle_automation_marquee_drag(pos, ui_state, viewport);
            }
            DragMode::AutomationGroupMove => {
                self.handle_automation_group_drag(pos, host, viewport);
            }
            DragMode::AutomationDraw => {
                self.write_automation_draw_step(pos, host, viewport);
            }
            DragMode::None => {}
        }
    }

    /// Port of Unity InteractionOverlay.OnEndDrag (lines 356-446).
    ///
    /// Takes no `UIState`: end-of-drag commits the engine batch and clears the
    /// overlay's own transient state — selection is untouched here.
    pub fn on_end_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        if crate::input::input_trace_enabled() {
            eprintln!("[input-trace] overlay: end_drag entered (mode={:?})", self.drag_mode);
        }
        // Unity lines 358-363: region select → finalize
        if self.drag_mode == DragMode::RegionSelect {
            host.invalidate_all_layer_bitmaps();
            self.drag_mode = DragMode::None;
            return;
        }

        // P4 Unit A: automation point drag → commit one undo entry (already
        // applied live by `set_automation_point_preview`, mirroring the clip
        // move path's own "already applied, just record" shape below — but
        // automation doesn't need the command-batch/enforce-non-overlap
        // machinery a clip move does, so it commits directly and returns.
        if self.drag_mode == DragMode::AutomationPoint {
            self.commit_automation_drag(host);
            self.drag_mode = DragMode::None;
            host.mark_dirty();
            host.set_cursor(TimelineCursor::Default);
            return;
        }

        // P4 Unit B: segment gestures commit the same way — already applied
        // live, just register the undo entry (single command for a bend,
        // batched pair for a vertical drag) — and return, same shape as the
        // automation-point path above.
        if self.drag_mode == DragMode::AutomationSegmentBend {
            self.commit_automation_segment_bend(host);
            self.drag_mode = DragMode::None;
            host.mark_dirty();
            host.set_cursor(TimelineCursor::Default);
            return;
        }
        if self.drag_mode == DragMode::AutomationSegmentDrag {
            self.commit_automation_segment_value_drag(host);
            self.drag_mode = DragMode::None;
            host.mark_dirty();
            host.set_cursor(TimelineCursor::Default);
            return;
        }

        // P4 Unit B: marquee selection isn't an edit — just stop tracking
        // and redraw (the selected set itself stays in `UIState`, already
        // written live during `on_drag`).
        if self.drag_mode == DragMode::AutomationMarquee {
            self.automation_marquee = None;
            self.drag_mode = DragMode::None;
            host.invalidate_all_layer_bitmaps();
            host.set_cursor(TimelineCursor::Default);
            return;
        }
        // P4 Unit B: group move / draw stroke commit the same way as the
        // single-point/segment gestures above — already applied live, one
        // undo entry, return.
        if self.drag_mode == DragMode::AutomationGroupMove {
            self.commit_automation_group_drag(host);
            self.drag_mode = DragMode::None;
            host.mark_dirty();
            host.set_cursor(TimelineCursor::Default);
            return;
        }
        if self.drag_mode == DragMode::AutomationDraw {
            self.commit_automation_draw(host);
            self.drag_mode = DragMode::None;
            host.mark_dirty();
            host.set_cursor(TimelineCursor::Default);
            return;
        }

        let ended_move = self.drag_mode == DragMode::Move;
        host.begin_command_batch();

        // D15 landing-line flash geometry — leftmost landed beat + layer span
        // of the clips that actually moved, accumulated in the record loop.
        let mut landed: Option<(Beats, usize, usize)> = None;

        if self.drag_mode == DragMode::Move {
            // Unity lines 370-386: record commands. No finalize-snap step —
            // `handle_move_drag` already snapped+clamped this position on the
            // last frame (D5); there is nothing left to reconcile here.
            for snapshot in &self.drag_snapshots {
                if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id) {
                    let start_changed =
                        (clip.start_beat - snapshot.start_beat).abs() >= Beats(0.0001);
                    let layer_changed = clip.layer_index != snapshot.layer_index;
                    if start_changed || layer_changed {
                        landed = Some(match landed {
                            None => (clip.start_beat, clip.layer_index, clip.layer_index),
                            Some((b, lo, hi)) => (
                                if clip.start_beat < b { clip.start_beat } else { b },
                                lo.min(clip.layer_index),
                                hi.max(clip.layer_index),
                            ),
                        });
                        host.record_move(
                            &snapshot.clip_id,
                            snapshot.start_beat,
                            clip.start_beat,
                            snapshot.layer_index,
                            clip.layer_index,
                        );
                        // Opt/Alt-drag: drop a copy back at the original position
                        // so the moved clip reads as the duplicate. Added to the
                        // same batch → one undo entry with the move.
                        if self.duplicate_on_release {
                            host.duplicate_clip_to(
                                &snapshot.clip_id,
                                snapshot.start_beat,
                                snapshot.layer_index,
                            );
                        }
                    }
                }
            }

            // Unity lines 407-416: enforce non-overlap on all dragged clips
            for snapshot in &self.drag_snapshots {
                host.enforce_non_overlap(&snapshot.clip_id, &self.drag_snapshot_clip_ids);
            }
        } else if self.drag_mode == DragMode::TrimLeft || self.drag_mode == DragMode::TrimRight {
            // Unity lines 390-401: record a trim command for every selected clip
            // that actually changed, each with its own pre/post geometry.
            for orig in &self.trim_originals {
                if let Some(clip) = host.find_clip_by_id(&orig.clip_id) {
                    let start_changed =
                        (clip.start_beat - orig.start_beat).abs() >= Beats(0.0001);
                    let dur_changed =
                        (clip.duration_beats - orig.duration_beats).abs() >= Beats(0.0001);
                    if start_changed || dur_changed {
                        host.record_trim(
                            &orig.clip_id,
                            orig.start_beat,
                            clip.start_beat,
                            orig.duration_beats,
                            clip.duration_beats,
                            orig.in_point,
                            clip.in_point,
                        );
                    }
                }
            }

            // Unity lines 417-421: enforce non-overlap on every trimmed clip,
            // ignoring the trimmed set itself so co-selected clips don't shove
            // each other (mirrors the move path's drag_snapshot_clip_ids).
            let trimmed_ids: HashSet<ClipId> =
                self.trim_originals.iter().map(|o| o.clip_id.clone()).collect();
            for id in &trimmed_ids {
                host.enforce_non_overlap(id, &trimmed_ids);
            }
        }

        // Unity lines 436-441: commit as composite command
        let desc = if ended_move {
            if self.duplicate_on_release {
                "Duplicate clips"
            } else {
                "Move clips"
            }
        } else {
            "Trim clips"
        };
        host.commit_command_batch(desc);

        // D15 landing-line flash — re-hooked at drag-end (2026-07-07). P1.4's
        // continuous snap deleted the discrete `finalize_move_snap` trigger
        // (see the dormancy note at the drawer, `app_render.rs`); the drag-END
        // commit is the new discrete moment. Fires once, only when a move
        // actually landed somewhere new (a click-without-move stays dark).
        // Move only — a trim reshapes in place, there is no "landing".
        // Feel sign-off owed to Peter (UI_CRAFT_AND_MOTION_PLAN.md D15 gate).
        if let Some((beat, lo, hi)) = landed {
            self.landing_flash_beat = beat;
            self.landing_flash_layers = (lo, hi);
            self.landing_flash.fire(color::MOTION_MED_MS);
        }

        // Unity lines 423-427/444-445: clear drag state
        self.reset_drag_state(host);
    }

    /// Escape cancels an in-flight move or trim gesture (D5, B8): the model
    /// is restored to the pre-gesture snapshot through the SAME begin/commit
    /// batch pair `on_end_drag` uses to land a real drag. Nothing is
    /// `record_*`'d into the batch, so `commit_command_batch` sees an empty
    /// command list and returns without pushing an undo entry or reaching the
    /// content thread (`AppEditingHost::commit_command_batch`'s
    /// `if commands.is_empty() { return; }`). This is "restore and close
    /// batch" — never "commit then undo": the latter would create a real
    /// undo entry only to erase it, which is observable (an extra undo-stack
    /// slot, a spurious `ContentCommand`) in a way a true cancel is not.
    ///
    /// Scope: move and trim only. Other in-flight gestures (region-select,
    /// automation editing) are untouched by this method — out of scope for
    /// P1.4 (D5/D8); callers should only invoke this when `drag_mode()` is
    /// `Move`, `TrimLeft`, or `TrimRight`.
    pub fn cancel_drag(&mut self, host: &mut dyn TimelineEditingHost) {
        match self.drag_mode {
            DragMode::Move => {
                for snapshot in &self.drag_snapshots {
                    host.set_clip_start_beat(&snapshot.clip_id, snapshot.start_beat);
                    if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id)
                        && clip.layer_index != snapshot.layer_index
                    {
                        host.move_clip_to_layer(&snapshot.clip_id, snapshot.layer_index);
                    }
                }
            }
            DragMode::TrimLeft | DragMode::TrimRight => {
                for orig in &self.trim_originals {
                    host.set_clip_trim(
                        &orig.clip_id,
                        orig.start_beat,
                        orig.duration_beats,
                        orig.in_point,
                    );
                }
            }
            _ => return,
        }
        // Restore-and-close: open/close the SAME batch pair `on_end_drag`
        // commits, but record nothing into it — the commit is a genuine
        // no-op (see doc comment above), not a commit-then-undo.
        host.begin_command_batch();
        host.commit_command_batch("cancelled");
        host.invalidate_all_layer_bitmaps();
        self.reset_drag_state(host);
    }

    /// The drag-state clear shared by a normal `on_end_drag` commit and an
    /// Escape `cancel_drag` — both end the gesture the same way once the
    /// model is settled (committed or restored).
    fn reset_drag_state(&mut self, host: &mut dyn TimelineEditingHost) {
        self.drag_mode = DragMode::None;
        self.drag_snapshots.clear();
        self.drag_snapshot_clip_ids.clear();
        self.drag_anchor_clip_id = None;
        self.trim_clip_id = None;
        self.trim_originals.clear();
        self.duplicate_on_release = false;
        self.drag_layer_blocked = false;
        host.mark_dirty();
        host.set_cursor(TimelineCursor::Default);
    }

    // ────────────────────────────────────────────────────────────
    // DRAG HANDLERS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.HandleMoveDrag (lines 463-537).
    fn handle_move_drag(
        &mut self,
        screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
    ) {
        if self.drag_anchor_clip_id.is_none() {
            return;
        }
        if self.drag_snapshots.is_empty() {
            self.capture_drag_selection(ui_state, host);
        }

        // Unity line 470: auto-scroll (B11) — the P0 scroll owner advances
        // BEFORE the beat conversion below, so this frame's gesture math
        // already reflects the new scroll position: a parked pointer still
        // advances the gesture as the content scrolls under it.
        viewport.autoscroll_edge(screen_pos);
        let mouse_beat = viewport.pixel_to_beat(screen_pos.x);

        // Unity lines 474-500: cross-layer delta
        let target_layer = viewport.layer_at_y(screen_pos.y);
        let mut layer_delta: i32 = 0;
        let total_layers = host.layer_count();

        if let Some(target) = target_layer
            && total_layers > 0
        {
            layer_delta = target as i32 - self.drag_start_layer_index as i32;
            let min_delta = -(self.drag_selection_min_layer as i32);
            let max_delta = (total_layers as i32 - 1) - self.drag_selection_max_layer as i32;
            layer_delta = layer_delta.clamp(min_delta, max_delta);

            // Unity lines 488-498: type compatibility check
            self.drag_layer_blocked = false;
            if layer_delta != 0 {
                for snapshot in &self.drag_snapshots {
                    let dest = snapshot.layer_index as i32 + layer_delta;
                    if dest < 0 || dest >= total_layers as i32 {
                        layer_delta = 0;
                        self.drag_layer_blocked = true;
                        break;
                    }
                    // Check video↔generator compatibility
                    let src_is_gen = host.layer_is_generator(snapshot.layer_index);
                    let dst_is_gen = host.layer_is_generator(dest as usize);
                    if src_is_gen != dst_is_gen {
                        layer_delta = 0;
                        self.drag_layer_blocked = true;
                        break;
                    }
                }
            }
        }

        // Unity lines 503-506: cursor feedback
        if self.drag_layer_blocked {
            host.set_cursor(TimelineCursor::Blocked);
        } else {
            host.set_cursor(TimelineCursor::Move);
        }

        // D17 error shake: fire once on the RISING edge of `drag_layer_blocked`
        // (a held-blocked drag must not re-fire every frame).
        if self.drag_layer_blocked && !self.was_layer_blocked {
            self.error_shake.fire(240.0);
        }
        self.was_layer_blocked = self.drag_layer_blocked;

        // Unity lines 508-520: apply cross-layer moves
        if layer_delta != 0 {
            for snapshot in &self.drag_snapshots {
                let target_layer = (snapshot.layer_index as i32 + layer_delta) as usize;
                if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id)
                    && target_layer != clip.layer_index
                {
                    host.move_clip_to_layer(&snapshot.clip_id, target_layer);
                }
            }
        }

        // Unity lines 522-534: magnetic snap + beat delta — shared with
        // (formerly) `finalize_move_snap`, see `move_snap_delta` (D5). The
        // clip written here IS the committed result: nothing left to reconcile
        // at release.
        let anchor_start_beat = mouse_beat - self.drag_offset_beats;
        let beat_delta = self.move_snap_delta(anchor_start_beat, viewport);

        // Apply beat delta to all clips (direct mutation during drag — committed in OnEndDrag)
        // Unity line 533: movingClip.StartBeat = Max(0, snapshot.StartBeat + beatDelta)
        for snapshot in &self.drag_snapshots {
            let new_start = (snapshot.start_beat + beat_delta).max(Beats::ZERO);
            host.set_clip_start_beat(&snapshot.clip_id, new_start);
        }

        host.invalidate_all_layer_bitmaps();
    }

    /// D5 — the shared snap+clamp math for a move drag. Given a candidate
    /// anchor beat, magnetic-snaps it against the grid and neighboring clip
    /// edges on the gesture's start layer (excluding the dragged clips
    /// themselves), then clamps the resulting delta so the group's leftmost
    /// clip cannot cross beat 0. `handle_move_drag` calls this every frame —
    /// the on-screen position IS the landed position, snap included.
    ///
    /// This used to be computed twice: once inline here, once inline in
    /// `finalize_move_snap` at release (reading the anchor's already-moved
    /// `start_beat` back from the host and re-snapping it — a no-op in
    /// practice since the per-frame value was already a valid snap
    /// candidate, but a second, independently-maintained implementation of
    /// the same math — exactly the "two authorities" bug class P0 named).
    /// `finalize_move_snap` is deleted; nothing was left for it to do that
    /// the last frame didn't already show (D5).
    ///
    /// B12: Cmd held mid-drag bypasses snap entirely (raw position) — checked
    /// here, at the ONE call site of the shared `magnetic_snap`, not by
    /// forking a second snap implementation. The floor clamp below is a
    /// separate invariant (D5) and still applies even when snap is bypassed.
    fn move_snap_delta(&self, candidate_anchor_beat: Beats, viewport: &TimelineViewportPanel) -> Beats {
        let snapped = if self.modifiers.command {
            candidate_anchor_beat
        } else {
            viewport.magnetic_snap(
                candidate_anchor_beat,
                self.drag_start_layer_index,
                &self
                    .drag_snapshot_clip_ids
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        let mut beat_delta = snapped - self.drag_start_beat;
        // Clamp: don't let the leftmost clip go below beat 0. The ONE shared
        // clamp site on the move path — the per-snapshot `.max(Beats::ZERO)`
        // below is the per-clip floor applied where the model is actually
        // written, not a second independent clamp.
        beat_delta = beat_delta.max(-self.drag_selection_min_start_beat);
        beat_delta
    }

    /// Port of Unity InteractionOverlay.HandleTrimLeftDrag (lines 539-560).
    fn handle_trim_left_drag(
        &mut self,
        screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &mut TimelineViewportPanel,
    ) {
        let trim_id = match &self.trim_clip_id {
            Some(id) => id.clone(),
            None => return,
        };

        // B11: autoscroll BEFORE the beat conversion, same ordering as
        // `handle_move_drag` — a parked pointer still advances the trim as
        // the content scrolls under it.
        viewport.autoscroll_edge(screen_pos);
        let mouse_beat = viewport.pixel_to_beat(screen_pos.x);

        let min_duration = Beats(0.25); // 1/16 note minimum (Unity line 544)
        let spb = host.get_seconds_per_beat() as f64;

        // Snap context comes from the grabbed clip; the resulting edge delta is
        // shared by every selected clip (each then clamps individually).
        // B12: Cmd held bypasses snap entirely (raw position) — same rule,
        // same shared `magnetic_snap` call, as the move path.
        let clip_layer = host.find_clip_by_id(&trim_id).map_or(0, |c| c.layer_index);
        let snapped = if self.modifiers.command {
            mouse_beat
        } else {
            viewport.magnetic_snap(mouse_beat, clip_layer, std::slice::from_ref(&trim_id))
        };
        let raw_delta = snapped - self.trim_original_start_beat;

        for orig in &self.trim_originals {
            let original_end = orig.start_beat + orig.duration_beats;
            // Video clips clamp to their own original start (in_point can't go
            // negative); generators extend left freely. (Unity lines 548-551.)
            let mut new_start = orig.start_beat + raw_delta;
            if !orig.is_generator {
                new_start = new_start.max(orig.start_beat);
            }
            new_start = new_start.min(original_end - min_duration);

            let beat_delta = new_start - orig.start_beat;
            let new_duration = original_end - new_start;
            let new_in_point =
                (orig.in_point + Seconds(beat_delta.0 * spb)).max(Seconds::ZERO);

            // Unity lines 554-557: direct mutation during drag
            host.set_clip_trim(&orig.clip_id, new_start, new_duration, new_in_point);
        }
        host.invalidate_all_layer_bitmaps();
    }

    /// Port of Unity InteractionOverlay.HandleTrimRightDrag (lines 562-582).
    fn handle_trim_right_drag(
        &mut self,
        screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        viewport: &mut TimelineViewportPanel,
    ) {
        let trim_id = match &self.trim_clip_id {
            Some(id) => id.clone(),
            None => return,
        };

        // B11: autoscroll BEFORE the beat conversion (see handle_trim_left_drag).
        viewport.autoscroll_edge(screen_pos);
        let mouse_beat = viewport.pixel_to_beat(screen_pos.x);

        let min_duration = Beats(0.25); // Unity line 566

        // Snap context from the grabbed clip; the edge delta fans over the
        // whole selection (each clip clamps individually). B12: Cmd bypasses
        // snap entirely (raw position) — same shared `magnetic_snap` call.
        let clip_layer = host.find_clip_by_id(&trim_id).map_or(0, |c| c.layer_index);
        let snapped = if self.modifiers.command {
            mouse_beat
        } else {
            viewport.magnetic_snap(mouse_beat, clip_layer, std::slice::from_ref(&trim_id))
        };
        let grabbed_original_end =
            self.trim_original_start_beat + self.trim_original_duration_beats;
        let raw_delta = snapped - grabbed_original_end;

        for orig in &self.trim_originals {
            let new_end = (orig.start_beat + orig.duration_beats + raw_delta)
                .max(orig.start_beat + min_duration);
            let mut new_duration = new_end - orig.start_beat;

            // Unity lines 573-578: clamp to video source length when not looping
            // (generators extend freely). in_point is unchanged by a right trim.
            if !orig.is_looping && !orig.is_generator {
                let max_dur = host.get_max_duration_beats(&orig.clip_id);
                if max_dur > Beats::ZERO {
                    new_duration = new_duration.min(max_dur);
                }
            }

            // Unity line 580: trimClip.DurationBeats = newDurationBeats
            host.set_clip_trim(&orig.clip_id, orig.start_beat, new_duration, orig.in_point);
        }
        host.invalidate_all_layer_bitmaps();
    }

    // ────────────────────────────────────────────────────────────
    // DRAG HELPERS
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.BeginMoveDrag (lines 592-660).
    fn begin_move_drag(
        &mut self,
        clip_id: &str,
        layer_index: usize,
        mouse_beat: Beats,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        _viewport: &TimelineViewportPanel,
    ) {
        self.drag_mode = DragMode::Move;

        // Unity lines 598-648: region-partial move
        if let Some(region) = ui_state.current_region().cloned()
            && let Some(clip) = host.find_clip_by_id(clip_id)
        {
            {
                let layer_in_region = host
                    .layer_id_at_index(clip.layer_index)
                    .is_some_and(|lid| region.contains_layer_id(&lid));
                let hit_in_region = clip.end_beat > region.start_beat
                    && clip.start_beat < region.end_beat
                    && layer_in_region;

                if hit_in_region {
                    let split_result = host.split_clips_for_region_move(&region);

                    // Find anchor among interior clips
                    let mut anchor_id = None;
                    for interior_id in &split_result.interior_clip_ids {
                        if let Some(ic) = host.find_clip_by_id(interior_id)
                            && ic.layer_index == layer_index
                            && mouse_beat >= ic.start_beat
                            && mouse_beat < ic.end_beat
                        {
                            anchor_id = Some(interior_id.clone());
                            break;
                        }
                    }
                    // Fallback: first interior clip on same layer
                    if anchor_id.is_none() {
                        for interior_id in &split_result.interior_clip_ids {
                            if let Some(ic) = host.find_clip_by_id(interior_id)
                                && ic.layer_index == layer_index
                            {
                                anchor_id = Some(interior_id.clone());
                                break;
                            }
                        }
                    }
                    // Fallback: first interior clip
                    if anchor_id.is_none() && !split_result.interior_clip_ids.is_empty() {
                        anchor_id = Some(split_result.interior_clip_ids[0].clone());
                    }

                    if let Some(anchor) = anchor_id
                        && let Some(ac) = host.find_clip_by_id(&anchor)
                    {
                        self.drag_anchor_clip_id = Some(anchor.clone());
                        self.drag_start_layer_index = ac.layer_index;
                        self.begin_move(ac.start_beat, mouse_beat);
                        self.capture_drag_selection_from_ids(&split_result.interior_clip_ids, host);
                        return;
                    }
                    // No interior clips — fall through to normal move
                }
            }
        }

        // Unity lines 650-659: normal move
        if !ui_state.is_selected(clip_id) {
            let lid = host.layer_id_at_index(layer_index).unwrap_or_default();
            ui_state.select_clip(ClipId::new(clip_id), lid);
            host.on_clip_selected(clip_id);
        }
        self.drag_anchor_clip_id = Some(ClipId::new(clip_id));
        self.drag_start_layer_index = layer_index;
        if let Some(clip) = host.find_clip_by_id(clip_id) {
            self.begin_move(clip.start_beat, mouse_beat);
        }
        self.capture_drag_selection(ui_state, host);
    }

    /// Port of Unity InteractionOverlay.CaptureDragSelection (lines 695-753).
    fn capture_drag_selection(&mut self, ui_state: &UIState, host: &dyn TimelineEditingHost) {
        self.drag_snapshots.clear();
        self.drag_snapshot_clip_ids.clear();

        let selected_ids = ui_state.get_selected_clip_ids();
        let mut found_any = false;

        for id in &selected_ids {
            if let Some(clip) = host.find_clip_by_id(id) {
                if clip.is_locked {
                    continue;
                }
                self.drag_snapshots.push(DragSnapshot {
                    clip_id: id.clone(),
                    start_beat: clip.start_beat,
                    layer_index: clip.layer_index,
                });
                self.drag_snapshot_clip_ids.insert(id.clone());

                if !found_any {
                    self.drag_selection_min_start_beat = clip.start_beat;
                    self.drag_selection_min_layer = clip.layer_index;
                    self.drag_selection_max_layer = clip.layer_index;
                    found_any = true;
                } else {
                    self.drag_selection_min_start_beat =
                        self.drag_selection_min_start_beat.min(clip.start_beat);
                    self.drag_selection_min_layer =
                        self.drag_selection_min_layer.min(clip.layer_index);
                    self.drag_selection_max_layer =
                        self.drag_selection_max_layer.max(clip.layer_index);
                }
            }
        }

        // Unity lines 740-753: fallback — anchor clip only
        if !found_any
            && let Some(ref anchor_id) = self.drag_anchor_clip_id
            && let Some(clip) = host.find_clip_by_id(anchor_id)
        {
            self.drag_snapshots.push(DragSnapshot {
                clip_id: anchor_id.clone(),
                start_beat: clip.start_beat,
                layer_index: clip.layer_index,
            });
            self.drag_snapshot_clip_ids.insert(anchor_id.clone());
            self.drag_selection_min_start_beat = clip.start_beat;
            self.drag_selection_min_layer = clip.layer_index;
            self.drag_selection_max_layer = clip.layer_index;
        }
    }

    /// Port of Unity InteractionOverlay.CaptureDragSelectionFromClips (lines 665-693).
    fn capture_drag_selection_from_ids(
        &mut self,
        clip_ids: &[ClipId],
        host: &dyn TimelineEditingHost,
    ) {
        self.drag_snapshots.clear();
        self.drag_snapshot_clip_ids.clear();

        if clip_ids.is_empty() {
            return;
        }

        let mut first = true;
        for id in clip_ids {
            if let Some(clip) = host.find_clip_by_id(id) {
                if clip.is_locked {
                    continue;
                }
                self.drag_snapshots.push(DragSnapshot {
                    clip_id: id.clone(),
                    start_beat: clip.start_beat,
                    layer_index: clip.layer_index,
                });
                self.drag_snapshot_clip_ids.insert(id.clone());

                if first {
                    self.drag_selection_min_start_beat = clip.start_beat;
                    self.drag_selection_min_layer = clip.layer_index;
                    self.drag_selection_max_layer = clip.layer_index;
                    first = false;
                } else {
                    self.drag_selection_min_start_beat =
                        self.drag_selection_min_start_beat.min(clip.start_beat);
                    self.drag_selection_min_layer =
                        self.drag_selection_min_layer.min(clip.layer_index);
                    self.drag_selection_max_layer =
                        self.drag_selection_max_layer.max(clip.layer_index);
                }
            }
        }
    }

    // ────────────────────────────────────────────────────────────
    // REGION SELECT
    // ────────────────────────────────────────────────────────────

    /// Port of Unity InteractionOverlay.BeginRegionDrag (lines 778-795).
    fn begin_region_drag(
        &mut self,
        press_pos: Vec2,
        ctrl_held: bool,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        self.region_drag_start_beat = viewport.pixel_to_beat(press_pos.x);
        self.region_drag_start_layer = viewport.layer_at_y(press_pos.y).unwrap_or(0);

        // Unity lines 793-794: clear selection unless Ctrl held
        if !ctrl_held {
            ui_state.clear_selection();
        }
    }

    /// Port of Unity InteractionOverlay.UpdateRegionDrag (lines 797-836).
    fn update_region_drag(
        &mut self,
        pos: Vec2,
        ui_state: &mut UIState,
        viewport: &mut TimelineViewportPanel,
        host: &dyn TimelineEditingHost,
    ) {
        // B11: edge autoscroll for rubber-band, same ordering as move/trim —
        // BEFORE the beat conversion below.
        viewport.autoscroll_edge(pos);
        let beat = viewport.pixel_to_beat(pos.x);
        let layer = viewport
            .layer_at_y(pos.y)
            .unwrap_or(self.region_drag_start_layer);

        let min_beat = self.region_drag_start_beat.min(beat);
        let max_beat = self.region_drag_start_beat.max(beat);
        let min_layer = self.region_drag_start_layer.min(layer);
        let max_layer = self.region_drag_start_layer.max(layer);

        // Unity lines 818-821: grid snap both edges
        let snapped_min = viewport.snap_to_grid(min_beat);
        let snapped_max = viewport.snap_to_grid(max_beat);

        // Unity line 835: update region live — bumps SelectionVersion
        ui_state.set_region(
            snapped_min,
            snapped_max,
            min_layer as i32,
            max_layer as i32,
            host.layers(),
        );
    }

    // ────────────────────────────────────────────────────────────
    // UTILITY
    // ────────────────────────────────────────────────────────────

    /// Hit-test at a screen position using the viewport's coordinate conversion.
    fn hit_test_at(&self, pos: Vec2, viewport: &TimelineViewportPanel) -> Option<ClipHitResult> {
        if !viewport.tracks_rect().contains(pos) {
            return None;
        }

        let beat = viewport.pixel_to_beat(pos.x).as_f32();
        let y_in_tracks = pos.y - viewport.tracks_rect().y;

        ClipHitTester::hit_test(
            beat,
            y_in_tracks + viewport.scroll_y_px(),
            self.clip_vertical_padding,
            viewport.mapper(),
            |layer_idx| viewport.clips_for_layer(layer_idx),
            |layer_idx| viewport.is_group_layer(layer_idx),
        )
    }

    /// Check if a clip is locked.
    fn clip_is_locked(&self, clip_id: &str, viewport: &TimelineViewportPanel) -> bool {
        (0..viewport.layer_count()).any(|i| {
            viewport
                .clips_for_layer(i)
                .iter()
                .any(|c| c.clip_id == clip_id && c.is_locked)
        })
    }
}

// ── B4 regression tests ─────────────────────────────────────────────
// `docs/TIMELINE_INTERACTION_P1_SPEC.md` D2 table, last row: a press on an
// already-selected clip must not collapse the selection until release
// without a drag; a drag begun on ANY selected member grabs the whole
// group. Reading `on_pointer_click`'s bare-click arm and
// `begin_move_drag`'s "normal move" arm (above) shows this is already
// correct by construction — `Click` and `DragBegin` are emitted as
// mutually exclusive terminal events by the input layer (`input.rs`;
// `DragBegin` fires once movement is detected, `Click` only when release
// comes with none), so nothing touches selection during the press itself
// either way; `begin_move_drag` only calls `select_clip` (collapsing) when
// the pressed clip is NOT already selected, and `capture_drag_selection`
// always fans over the CURRENT selection set. No production change was
// needed for B4 — these tests pin the contract through the real
// `on_pointer_click`/`on_begin_drag` entry points instead of leaving it
// implicit.
#[cfg(test)]
mod b4_group_move_tests {
    use super::*;
    use crate::layout::ScreenLayout;
    use crate::panels::Panel;
    use crate::panels::viewport::{TimelineViewportPanel, TrackInfo, ViewportClip};
    use crate::timeline_editing_host::RegionSplitResult;
    use crate::tree::UITree;
    use crate::types::LayerType;
    use crate::view::{SelectionRegion, UiLayer};
    use manifold_foundation::LayerId;

    /// Minimal host — only the handful of methods `on_pointer_click`'s and
    /// `on_begin_drag`'s Body-hit arms actually read (clip/layer lookup) are
    /// meaningfully implemented; everything else is a harmless no-op. A full
    /// mock of the ~40-method trait was judged too costly for the S2 repro
    /// in P1.0's evidence deck; this one is scoped to exactly what these two
    /// entry points touch for a plain (non-automation, non-region-partial)
    /// clip press.
    struct TestHost {
        layers: Vec<UiLayer>,
        // (id, layer_index, start_beat, duration_beats, is_locked)
        clips: Vec<(ClipId, usize, Beats, Beats, bool)>,
    }

    impl TestHost {
        fn new(layer_ids: &[&str]) -> Self {
            let layers = layer_ids
                .iter()
                .map(|id| UiLayer {
                    layer_id: LayerId::new(id),
                    parent_layer_id: None,
                    layer_type: LayerType::Video,
                    is_collapsed: false,
                    automation_lane_count: 0,
                })
                .collect();
            Self {
                layers,
                clips: Vec::new(),
            }
        }

        fn with_clip(mut self, id: &str, layer_index: usize, start: f32, duration: f32) -> Self {
            self.clips.push((
                ClipId::new(id),
                layer_index,
                Beats::from_f32(start),
                Beats::from_f32(duration),
                false,
            ));
            self
        }

        fn to_ref(&self, entry: &(ClipId, usize, Beats, Beats, bool)) -> ClipRef {
            let (id, li, start, dur, locked) = entry;
            ClipRef {
                clip_id: id.clone(),
                start_beat: *start,
                duration_beats: *dur,
                end_beat: *start + *dur,
                layer_index: *li,
                layer_id: self.layers[*li].layer_id.clone(),
                in_point: Seconds::ZERO,
                is_generator: false,
                is_locked: *locked,
                is_looping: false,
            }
        }
    }

    impl TimelineEditingHost for TestHost {
        fn layer_count(&self) -> usize {
            self.layers.len()
        }
        fn layers(&self) -> &[UiLayer] {
            &self.layers
        }
        fn layer_id_at_index(&self, index: usize) -> Option<LayerId> {
            self.layers.get(index).map(|l| l.layer_id.clone())
        }
        fn layer_is_generator(&self, _index: usize) -> bool {
            false
        }
        fn is_layer_muted(&self, _index: usize) -> bool {
            false
        }
        fn project_beats_per_bar(&self) -> u32 {
            4
        }
        fn get_seconds_per_beat(&self) -> f32 {
            0.5
        }
        fn is_playing(&self) -> bool {
            false
        }
        fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef> {
            self.clips
                .iter()
                .find(|c| c.0.as_str() == clip_id)
                .map(|c| self.to_ref(c))
        }
        fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef> {
            self.clips
                .iter()
                .filter(|c| c.1 == layer_index)
                .map(|c| self.to_ref(c))
                .collect()
        }
        fn screen_position_to_beat(&self, _pos: Vec2) -> Beats {
            Beats::ZERO
        }
        fn get_layer_index_at_position(&self, _pos: Vec2) -> Option<usize> {
            None
        }
        fn beat_to_time(&self, _beat: Beats) -> Seconds {
            Seconds::ZERO
        }
        fn create_clip_at_position(
            &mut self,
            _beat: Beats,
            _layer: usize,
            _grid_step: Beats,
        ) -> Option<ClipId> {
            None
        }
        fn move_clip_to_layer(&mut self, _clip_id: &str, _target_layer: usize) {}
        fn on_clip_selected(&mut self, _clip_id: &str) {}
        fn on_clip_right_click(&mut self, _clip_id: &str, _screen_pos: Vec2) {}
        fn on_track_right_click(&mut self, _beat: Beats, _layer_index: usize, _screen_pos: Vec2) {}
        fn inspect_layer(&mut self, _layer_index: usize) {}
        fn invalidate_layer_bitmap(&mut self, _layer_index: usize) {}
        fn invalidate_all_layer_bitmaps(&mut self) {}
        fn mark_dirty(&mut self) {}
        fn set_cursor(&mut self, _cursor: TimelineCursor) {}
        fn scrub_to_time(&mut self, _time: Seconds) {}
        fn enforce_non_overlap(&mut self, _clip_id: &str, _ignore_ids: &HashSet<ClipId>) {}
        fn split_clips_for_region_move(&mut self, _region: &SelectionRegion) -> RegionSplitResult {
            RegionSplitResult {
                interior_clip_ids: Vec::new(),
                split_count: 0,
            }
        }
        fn begin_command_batch(&mut self) {}
        fn record_move(
            &mut self,
            _clip_id: &str,
            _old_start: Beats,
            _new_start: Beats,
            _old_layer: usize,
            _new_layer: usize,
        ) {
        }
        fn record_trim(
            &mut self,
            _clip_id: &str,
            _old_start: Beats,
            _new_start: Beats,
            _old_duration: Beats,
            _new_duration: Beats,
            _old_in_point: Seconds,
            _new_in_point: Seconds,
        ) {
        }
        fn duplicate_clip_to(&mut self, _src_clip_id: &str, _target_beat: Beats, _target_layer: usize) {}
        fn commit_command_batch(&mut self, _description: &str) {}
        fn set_clip_start_beat(&mut self, _clip_id: &str, _beat: Beats) {}
        fn set_clip_trim(
            &mut self,
            _clip_id: &str,
            _start_beat: Beats,
            _duration_beats: Beats,
            _in_point: Seconds,
        ) {
        }
        fn get_max_duration_beats(&self, _clip_id: &str) -> Beats {
            Beats::ZERO
        }
        fn add_automation_point(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _beat: Beats,
            _value: f32,
            _shape: UiSegmentShape,
        ) {
        }
        fn set_automation_point_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _from_beat: Beats,
            _to_beat: Beats,
            _to_value: f32,
        ) {
        }
        fn commit_automation_point_move(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _old: (Beats, f32, UiSegmentShape),
            _new: (Beats, f32, UiSegmentShape),
        ) {
        }
        fn remove_automation_point(&mut self, _target: &UiGraphTarget, _param_id: &ParamId, _beat: Beats) {}
        fn set_automation_segment_bend_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _left_beat: Beats,
            _bend: f32,
        ) {
        }
        fn set_automation_segment_drag_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _left_beat: Beats,
            _left_value: f32,
            _right_beat: Beats,
            _right_value: f32,
        ) {
        }
        fn commit_automation_segment_drag(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _left: (Beats, f32, f32, UiSegmentShape),
            _right: (Beats, f32, f32, UiSegmentShape),
        ) {
        }
        fn commit_automation_group_move(
            &mut self,
            _moves: Vec<(UiGraphTarget, ParamId, Beats, f32, f32, UiSegmentShape)>,
        ) {
        }
        fn automation_lane_points(
            &self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
        ) -> Option<Vec<(Beats, f32, UiSegmentShape)>> {
            None
        }
        fn set_automation_draw_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _points: Vec<(Beats, f32, UiSegmentShape)>,
        ) {
        }
        fn commit_automation_draw_stroke(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _new_points: Vec<(Beats, f32, UiSegmentShape)>,
            _old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
        ) {
        }
    }

    fn test_clip(id: &str, layer_index: usize, start: f32, duration: f32) -> ViewportClip {
        ViewportClip {
            clip_id: id.into(),
            layer_index,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(duration),
            name: "".into(),
            color: crate::color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
        }
    }

    /// Two abutting clips (`clip_a` [0,4), `clip_b` [4,8)) on one layer, built
    /// through the REAL `Panel::build` so the geometry matches production.
    fn build_two_clip_viewport() -> TimelineViewportPanel {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(vec![TrackInfo::default()]);
        panel.set_clips(vec![
            test_clip("clip_a", 0, 0.0, 4.0),
            test_clip("clip_b", 0, 4.0, 4.0),
        ]);
        // The mapper's Y-layout (what `get_layer_at_y` — and so hit-testing —
        // reads) is rebuilt from real layer data, separately from `build()`'s
        // own tracks_rect/bitmap geometry (app_render.rs does both on every
        // structural sync). Skipping this leaves `mapper.get_layer_at_y`
        // empty even though the painted rects look right.
        panel.rebuild_mapper_layout(&[UiLayer {
            layer_id: LayerId::new("layer-0"),
            parent_layer_id: None,
            layer_type: LayerType::Video,
            is_collapsed: false,
            automation_lane_count: 0,
        }]);
        let layout = ScreenLayout::new(1920.0, 1080.0);
        panel.build(&mut tree, &layout);
        panel
    }

    /// Screen position over the body of whichever clip covers `beat_center`,
    /// vertically centered in layer 0's row.
    fn body_pos_for(panel: &TimelineViewportPanel, beat_center: f32) -> Vec2 {
        Vec2::new(
            panel.beat_to_pixel(Beats::from_f32(beat_center)),
            panel.tracks_rect().y + 70.0,
        )
    }

    #[test]
    fn press_and_release_without_drag_collapses_to_the_clicked_clip() {
        let panel = build_two_clip_viewport();
        let mut host = TestHost::new(&["layer-0"])
            .with_clip("clip_a", 0, 0.0, 4.0)
            .with_clip("clip_b", 0, 4.0, 4.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a"), ClipId::new("clip_b")]);

        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        // A plain click (mouse-up-without-drag) on clip_a, part of the
        // existing multi-selection — must collapse to just this clip.
        overlay.on_pointer_click(
            body_pos_for(&panel, 2.0),
            false,
            false,
            1,
            false,
            &mut host,
            &mut ui_state,
            &panel,
        );

        let ids: HashSet<ClipId> = ui_state.get_selected_clip_ids().into_iter().collect();
        assert_eq!(
            ids,
            HashSet::from([ClipId::new("clip_a")]),
            "a plain click with no drag collapses the group to the clicked clip"
        );
    }

    #[test]
    fn drag_from_any_selected_member_moves_the_whole_group() {
        let panel = build_two_clip_viewport();
        let mut host = TestHost::new(&["layer-0"])
            .with_clip("clip_a", 0, 0.0, 4.0)
            .with_clip("clip_b", 0, 4.0, 4.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a"), ClipId::new("clip_b")]);

        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        // Press on clip_b — NOT the anchor — and begin a drag: grab-any-member.
        overlay.on_begin_drag(
            body_pos_for(&panel, 6.0),
            &mut host,
            &mut ui_state,
            &panel,
        );

        assert_eq!(overlay.drag_mode(), DragMode::Move);
        let ids: HashSet<ClipId> = ui_state.get_selected_clip_ids().into_iter().collect();
        let expected = HashSet::from([ClipId::new("clip_a"), ClipId::new("clip_b")]);
        assert_eq!(
            ids, expected,
            "a drag begun on any already-selected member keeps the whole group selected"
        );
    }
}

// ── P1.4 gesture integrity tests ─────────────────────────────────────
// `docs/TIMELINE_INTERACTION_P1_SPEC.md` D5 (preview == committed result,
// per-frame snap+clamp, Escape restores the pre-gesture snapshot) and D8
// (4px drag threshold).
#[cfg(test)]
mod p1_4_gesture_integrity_tests {
    use super::*;
    use crate::input::{PointerAction as InputPointerAction, UIEvent, UIInputSystem};
    use crate::layout::ScreenLayout;
    use crate::panels::Panel;
    use crate::panels::viewport::{TimelineViewportPanel, TrackInfo, ViewportClip};
    use crate::timeline_editing_host::RegionSplitResult;
    use crate::node::UIStyle;
    use crate::tree::UITree;
    use crate::types::LayerType;
    use crate::view::{SelectionRegion, UiLayer};
    use manifold_foundation::LayerId;

    /// A move/trim-capable, mutation-tracking host. Unlike
    /// `b4_group_move_tests::TestHost` (whose `set_clip_start_beat`/
    /// `move_clip_to_layer` are no-ops — sufficient for that module's
    /// selection-only assertions), P1.4's tests need to observe the actual
    /// live model mutation a drag performs, and what `record_move`/
    /// `record_trim`/`commit_command_batch` do with it — so every mutating
    /// method here is real, and `commit_command_batch` mirrors
    /// `AppEditingHost`'s own `if commands.is_empty() { return; }`
    /// short-circuit (an empty batch pushes no undo entry).
    #[derive(Clone, Debug, PartialEq)]
    struct ClipEntry {
        id: ClipId,
        layer_index: usize,
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
    }

    struct GestureTestHost {
        layers: Vec<UiLayer>,
        clips: Vec<ClipEntry>,
        move_records: Vec<(ClipId, Beats, Beats, usize, usize)>,
        trim_records: Vec<(ClipId, Beats, Beats, Beats, Beats)>,
        batch_ops: usize,
        committed_batches: Vec<usize>,
    }

    impl GestureTestHost {
        fn new(layer_ids: &[&str]) -> Self {
            let layers = layer_ids
                .iter()
                .map(|id| UiLayer {
                    layer_id: LayerId::new(id),
                    parent_layer_id: None,
                    layer_type: LayerType::Video,
                    is_collapsed: false,
                    automation_lane_count: 0,
                })
                .collect();
            Self {
                layers,
                clips: Vec::new(),
                move_records: Vec::new(),
                trim_records: Vec::new(),
                batch_ops: 0,
                committed_batches: Vec::new(),
            }
        }

        fn with_clip(mut self, id: &str, layer_index: usize, start: f32, duration: f32) -> Self {
            self.clips.push(ClipEntry {
                id: ClipId::new(id),
                layer_index,
                start_beat: Beats::from_f32(start),
                duration_beats: Beats::from_f32(duration),
                in_point: Seconds::ZERO,
            });
            self
        }

        fn to_ref(&self, e: &ClipEntry) -> ClipRef {
            ClipRef {
                clip_id: e.id.clone(),
                start_beat: e.start_beat,
                duration_beats: e.duration_beats,
                end_beat: e.start_beat + e.duration_beats,
                layer_index: e.layer_index,
                layer_id: self.layers[e.layer_index].layer_id.clone(),
                in_point: e.in_point,
                is_generator: false,
                is_locked: false,
                is_looping: false,
            }
        }
    }

    impl TimelineEditingHost for GestureTestHost {
        fn layer_count(&self) -> usize {
            self.layers.len()
        }
        fn layers(&self) -> &[UiLayer] {
            &self.layers
        }
        fn layer_id_at_index(&self, index: usize) -> Option<LayerId> {
            self.layers.get(index).map(|l| l.layer_id.clone())
        }
        fn layer_is_generator(&self, _index: usize) -> bool {
            false
        }
        fn is_layer_muted(&self, _index: usize) -> bool {
            false
        }
        fn project_beats_per_bar(&self) -> u32 {
            4
        }
        fn get_seconds_per_beat(&self) -> f32 {
            0.5
        }
        fn is_playing(&self) -> bool {
            false
        }
        fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef> {
            self.clips
                .iter()
                .find(|c| c.id.as_str() == clip_id)
                .map(|c| self.to_ref(c))
        }
        fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef> {
            self.clips
                .iter()
                .filter(|c| c.layer_index == layer_index)
                .map(|c| self.to_ref(c))
                .collect()
        }
        fn screen_position_to_beat(&self, _pos: Vec2) -> Beats {
            Beats::ZERO
        }
        fn get_layer_index_at_position(&self, _pos: Vec2) -> Option<usize> {
            None
        }
        fn beat_to_time(&self, _beat: Beats) -> Seconds {
            Seconds::ZERO
        }
        fn create_clip_at_position(
            &mut self,
            _beat: Beats,
            _layer: usize,
            _grid_step: Beats,
        ) -> Option<ClipId> {
            None
        }
        fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize) {
            if let Some(c) = self.clips.iter_mut().find(|c| c.id.as_str() == clip_id) {
                c.layer_index = target_layer;
            }
        }
        fn on_clip_selected(&mut self, _clip_id: &str) {}
        fn on_clip_right_click(&mut self, _clip_id: &str, _screen_pos: Vec2) {}
        fn on_track_right_click(&mut self, _beat: Beats, _layer_index: usize, _screen_pos: Vec2) {}
        fn inspect_layer(&mut self, _layer_index: usize) {}
        fn invalidate_layer_bitmap(&mut self, _layer_index: usize) {}
        fn invalidate_all_layer_bitmaps(&mut self) {}
        fn mark_dirty(&mut self) {}
        fn set_cursor(&mut self, _cursor: TimelineCursor) {}
        fn scrub_to_time(&mut self, _time: Seconds) {}
        fn enforce_non_overlap(&mut self, _clip_id: &str, _ignore_ids: &HashSet<ClipId>) {}
        fn split_clips_for_region_move(&mut self, _region: &SelectionRegion) -> RegionSplitResult {
            RegionSplitResult {
                interior_clip_ids: Vec::new(),
                split_count: 0,
            }
        }
        fn begin_command_batch(&mut self) {
            self.batch_ops = 0;
        }
        fn record_move(
            &mut self,
            clip_id: &str,
            old_start: Beats,
            new_start: Beats,
            old_layer: usize,
            new_layer: usize,
        ) {
            self.move_records
                .push((ClipId::new(clip_id), old_start, new_start, old_layer, new_layer));
            self.batch_ops += 1;
        }
        fn record_trim(
            &mut self,
            clip_id: &str,
            old_start: Beats,
            new_start: Beats,
            old_duration: Beats,
            new_duration: Beats,
            _old_in_point: Seconds,
            _new_in_point: Seconds,
        ) {
            self.trim_records
                .push((ClipId::new(clip_id), old_start, new_start, old_duration, new_duration));
            self.batch_ops += 1;
        }
        fn duplicate_clip_to(&mut self, _src_clip_id: &str, _target_beat: Beats, _target_layer: usize) {
            self.batch_ops += 1;
        }
        fn commit_command_batch(&mut self, _description: &str) {
            // Mirrors `AppEditingHost::commit_command_batch`'s
            // `if commands.is_empty() { return; }` — a batch with nothing
            // recorded produces no undo entry and reaches nothing downstream.
            if self.batch_ops > 0 {
                self.committed_batches.push(self.batch_ops);
            }
            self.batch_ops = 0;
        }
        fn set_clip_start_beat(&mut self, clip_id: &str, beat: Beats) {
            if let Some(c) = self.clips.iter_mut().find(|c| c.id.as_str() == clip_id) {
                c.start_beat = beat;
            }
        }
        fn set_clip_trim(
            &mut self,
            clip_id: &str,
            start_beat: Beats,
            duration_beats: Beats,
            in_point: Seconds,
        ) {
            if let Some(c) = self.clips.iter_mut().find(|c| c.id.as_str() == clip_id) {
                c.start_beat = start_beat;
                c.duration_beats = duration_beats;
                c.in_point = in_point;
            }
        }
        fn get_max_duration_beats(&self, _clip_id: &str) -> Beats {
            Beats::ZERO
        }
        fn add_automation_point(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _beat: Beats,
            _value: f32,
            _shape: UiSegmentShape,
        ) {
        }
        fn set_automation_point_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _from_beat: Beats,
            _to_beat: Beats,
            _to_value: f32,
        ) {
        }
        fn commit_automation_point_move(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _old: (Beats, f32, UiSegmentShape),
            _new: (Beats, f32, UiSegmentShape),
        ) {
        }
        fn remove_automation_point(&mut self, _target: &UiGraphTarget, _param_id: &ParamId, _beat: Beats) {}
        fn set_automation_segment_bend_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _left_beat: Beats,
            _bend: f32,
        ) {
        }
        fn set_automation_segment_drag_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _left_beat: Beats,
            _left_value: f32,
            _right_beat: Beats,
            _right_value: f32,
        ) {
        }
        fn commit_automation_segment_drag(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _left: (Beats, f32, f32, UiSegmentShape),
            _right: (Beats, f32, f32, UiSegmentShape),
        ) {
        }
        fn commit_automation_group_move(
            &mut self,
            _moves: Vec<(UiGraphTarget, ParamId, Beats, f32, f32, UiSegmentShape)>,
        ) {
        }
        fn automation_lane_points(
            &self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
        ) -> Option<Vec<(Beats, f32, UiSegmentShape)>> {
            None
        }
        fn set_automation_draw_preview(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _points: Vec<(Beats, f32, UiSegmentShape)>,
        ) {
        }
        fn commit_automation_draw_stroke(
            &mut self,
            _target: &UiGraphTarget,
            _param_id: &ParamId,
            _new_points: Vec<(Beats, f32, UiSegmentShape)>,
            _old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
        ) {
        }
    }

    fn test_clip(id: &str, layer_index: usize, start: f32, duration: f32) -> ViewportClip {
        ViewportClip {
            clip_id: id.into(),
            layer_index,
            start_beat: Beats::from_f32(start),
            duration_beats: Beats::from_f32(duration),
            name: "".into(),
            color: crate::color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            warped_secs_per_beat: 0.0,
        }
    }

    /// One clip `[0,8)` and a second `[16,20)` on one layer — the second
    /// gives magnetic snap a neighbor edge to pull toward. Built through the
    /// REAL `Panel::build` so beat<->pixel geometry matches production.
    fn build_viewport() -> TimelineViewportPanel {
        let mut tree = UITree::new();
        let mut panel = TimelineViewportPanel::new();
        panel.set_tracks(vec![TrackInfo::default()]);
        panel.set_clips(vec![
            test_clip("clip_a", 0, 0.0, 8.0),
            test_clip("clip_b", 0, 16.0, 4.0),
        ]);
        panel.rebuild_mapper_layout(&[UiLayer {
            layer_id: LayerId::new("layer-0"),
            parent_layer_id: None,
            layer_type: LayerType::Video,
            is_collapsed: false,
            automation_lane_count: 0,
        }]);
        let layout = ScreenLayout::new(1920.0, 1080.0);
        panel.build(&mut tree, &layout);
        panel
    }

    fn body_pos_for(panel: &TimelineViewportPanel, beat_center: f32) -> Vec2 {
        Vec2::new(
            panel.beat_to_pixel(Beats::from_f32(beat_center)),
            panel.tracks_rect().y + 70.0,
        )
    }

    /// D5 core assertion: drive several synthetic pointer moves, then compare
    /// the model's position after the LAST move (the on-screen "preview") to
    /// its position after `on_end_drag` commits (the "committed result").
    /// Run twice per the gate's "snap on and off": once landing close enough
    /// to `clip_b`'s start (16.0) that the neighbor-edge candidate wins over
    /// plain grid-snap, once landing far from any clip edge (grid-snap only —
    /// grid-snap itself has no "off" state today, per `magnetic_snap`'s
    /// half-grid-interval full-coverage threshold; B12's Cmd-bypass toggle is
    /// P1.5). Both must show zero further change at commit.
    fn assert_preview_equals_committed(landing_beat_center: f32, case: &str) {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::Move, "case {case}: drag must begin");

        // Several synthetic pointer moves walking toward the landing spot.
        for step in 1..=3 {
            let t = step as f32 / 3.0;
            let beat = 2.0 + (landing_beat_center - 2.0) * t;
            overlay.on_drag(body_pos_for(&panel, beat), &mut host, &mut ui_state, &mut panel);
        }

        let preview = host.find_clip_by_id("clip_a").unwrap().start_beat;
        overlay.on_end_drag(&mut host);
        let committed = host.find_clip_by_id("clip_a").unwrap().start_beat;

        assert_eq!(
            preview, committed,
            "case {case}: on-screen preview must already be the committed result (D5)"
        );
    }

    #[test]
    fn preview_equals_committed_neighbor_edge_snap_wins() {
        assert_preview_equals_committed(15.9, "neighbor-edge snap wins over grid");
    }

    #[test]
    fn preview_equals_committed_grid_snap_only() {
        assert_preview_equals_committed(9.37, "grid-snap only, no neighbor edge in range");
    }

    #[test]
    fn escape_mid_drag_restores_byte_identical_state() {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        let before = host.clips.clone();

        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        overlay.on_drag(body_pos_for(&panel, 10.0), &mut host, &mut ui_state, &mut panel);
        overlay.on_drag(body_pos_for(&panel, 12.37), &mut host, &mut ui_state, &mut panel);

        // Sanity: the drag actually moved the clip before cancelling —
        // otherwise this test would pass for the wrong reason.
        assert_ne!(
            host.find_clip_by_id("clip_a").unwrap().start_beat,
            Beats::from_f32(0.0),
            "sanity: the drag must have actually moved the clip"
        );

        overlay.cancel_drag(&mut host);

        assert_eq!(
            host.clips, before,
            "Escape must restore byte-identical clip state"
        );
        assert_eq!(overlay.drag_mode(), DragMode::None);
        assert!(
            host.committed_batches.is_empty(),
            "cancel must not push an undo entry — restore-and-close-batch, never commit-then-undo"
        );
        assert!(
            host.move_records.is_empty(),
            "cancel must not record a move"
        );
    }

    #[test]
    fn sub_four_px_press_release_moves_nothing() {
        let panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        // A widget under the press position so `UIInputSystem`'s gesture
        // recognizer (the real 4px threshold — `input.rs`'s `DRAG_THRESHOLD`)
        // has something to press/release on. Its identity is irrelevant: only
        // the emitted event kind + `pos` matter downstream — InteractionOverlay
        // does its own beat-space hit-testing independent of the UITree node
        // system, exactly as production does (viewport events are stashed by
        // node-based `UIInputSystem` events but interpreted by beat/pixel math).
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 1920.0, 1080.0, UIStyle::default());
        let press_pos = body_pos_for(&panel, 2.0);
        tree.add_button(
            Some(root),
            press_pos.x - 50.0,
            press_pos.y - 20.0,
            100.0,
            40.0,
            UIStyle::default(),
            "",
        );

        let mut input_sys = UIInputSystem::new();
        input_sys.process_pointer(&mut tree, press_pos, InputPointerAction::Down, 0.0);
        input_sys.process_pointer(
            &mut tree,
            press_pos + Vec2::new(2.0, 1.0),
            InputPointerAction::Move,
            0.0,
        );
        input_sys.process_pointer(
            &mut tree,
            press_pos + Vec2::new(2.0, 1.0),
            InputPointerAction::Up,
            0.0,
        );
        let events = input_sys.drain_events();

        assert!(
            !events.iter().any(|e| matches!(
                e,
                UIEvent::DragBegin { .. } | UIEvent::Drag { .. } | UIEvent::DragEnd { .. }
            )),
            "sub-4px press-release must not produce a drag event, got {events:?}"
        );

        // Feed exactly what the input layer produced into the overlay — the
        // ONLY entry points that ever mutate a clip's position
        // (`on_begin_drag`/`on_drag`) are never called on this path, by
        // construction, since no DragBegin/Drag event was ever emitted.
        for event in &events {
            if let UIEvent::Click { pos, modifiers, .. } = event {
                overlay.on_pointer_click(
                    *pos,
                    modifiers.shift,
                    modifiers.ctrl || modifiers.command,
                    1,
                    false,
                    &mut host,
                    &mut ui_state,
                    &panel,
                );
            }
        }

        assert_eq!(
            host.find_clip_by_id("clip_a").unwrap().start_beat,
            Beats::ZERO,
            "start_beat must be unchanged by a sub-threshold press-release"
        );
        assert!(
            host.move_records.is_empty(),
            "no move should be recorded for a sub-threshold press-release"
        );
    }

    #[test]
    fn full_drag_produces_exactly_one_undo_entry() {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        overlay.on_drag(body_pos_for(&panel, 6.0), &mut host, &mut ui_state, &mut panel);
        overlay.on_drag(body_pos_for(&panel, 10.0), &mut host, &mut ui_state, &mut panel);
        overlay.on_end_drag(&mut host);

        assert_eq!(
            host.committed_batches.len(),
            1,
            "exactly one non-empty commit_command_batch call must fire per gesture (B9)"
        );
        assert_eq!(host.move_records.len(), 1, "one clip moved -> one record_move call");
    }

    #[test]
    fn drag_toward_zero_clamps_every_frame_not_just_at_release() {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 2.0, 4.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        overlay.on_begin_drag(body_pos_for(&panel, 4.0), &mut host, &mut ui_state, &panel);

        // Walk the pointer far past beat 0 in several steps; the clamp must
        // hold on EVERY intermediate frame, not just once the drag ends.
        for beat in [3.0, 0.5, -2.0, -8.0, -20.0] {
            overlay.on_drag(body_pos_for(&panel, beat), &mut host, &mut ui_state, &mut panel);
            let start = host.find_clip_by_id("clip_a").unwrap().start_beat;
            assert!(
                start >= Beats::ZERO,
                "start_beat went negative mid-drag: {start:?} at pointer beat {beat}"
            );
        }

        overlay.on_end_drag(&mut host);
        let start = host.find_clip_by_id("clip_a").unwrap().start_beat;
        assert!(start >= Beats::ZERO, "start_beat negative after release: {start:?}");
    }

    // ── P1.5 drag ergonomics tests ────────────────────────────────────
    // `docs/TIMELINE_INTERACTION_P1_SPEC.md` B11 (edge autoscroll),
    // B12 (clip-edge/marker snap targets + Cmd-bypass).

    /// B11: a pointer parked at the viewport's right edge must keep
    /// advancing the (single) horizontal scroll offset frame over frame
    /// with NO further pointer movement, and the gesture's beat mapping
    /// must track it — the dragged clip keeps moving even though the
    /// screen position never changes, because the same screen x now maps
    /// to an advancing beat as the content scrolls under it.
    #[test]
    fn edge_autoscroll_advances_scroll_and_gesture_tracks_it() {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        assert_eq!(overlay.drag_mode(), DragMode::Move, "drag must begin");

        // Parked just inside the right edge of the tracks area — within
        // `TimelineViewportPanel::AUTOSCROLL_EDGE_PX` of `tracks_rect.x_max()`.
        let edge_pos = Vec2::new(panel.tracks_rect().x_max() - 5.0, panel.tracks_rect().y + 70.0);

        let scroll_start = panel.scroll_x_beats();
        let start_begin = host.find_clip_by_id("clip_a").unwrap().start_beat;
        let mut last_scroll = scroll_start;
        let mut last_start = start_begin;

        for frame in 0..8 {
            // `on_drag` here stands in for both a real mouse-move event AND
            // the stationary-pointer poll (`poll_drag`) — both funnel into
            // `handle_move_drag`, which is where `autoscroll_edge` lives.
            overlay.on_drag(edge_pos, &mut host, &mut ui_state, &mut panel);
            let scroll_now = panel.scroll_x_beats();
            let start_now = host.find_clip_by_id("clip_a").unwrap().start_beat;
            assert!(
                scroll_now >= last_scroll,
                "frame {frame}: scroll must never move backward while parked at the edge (was {last_scroll:?}, now {scroll_now:?})"
            );
            assert!(
                start_now >= last_start,
                "frame {frame}: the dragged clip's model position must track the scrolling view (was {last_start:?}, now {start_now:?})"
            );
            last_scroll = scroll_now;
            last_start = start_now;
        }

        assert!(
            last_scroll > scroll_start,
            "scroll must have actually advanced over 8 parked frames (started at {scroll_start:?}, ended at {last_scroll:?})"
        );
        assert!(
            last_start > start_begin,
            "the gesture must have actually advanced the clip over 8 parked frames (started at {start_begin:?}, ended at {last_start:?})"
        );
    }

    /// B12: with snap on, a dragged clip lands flush against a neighbor's
    /// edge; with Cmd held mid-drag, the same gesture must NOT snap (raw
    /// position). Fixture: `clip_a` `[0,8)`, `clip_b` `[16,20)` (from
    /// `build_viewport`). Press `clip_a` at mouse-beat 2.0 (so
    /// `drag_offset_beats` = 2.0 - 0.0 = 2.0) and land at mouse-beat 17.9,
    /// so the candidate anchor is 17.9 - 2.0 = 15.9 — within snap range of
    /// `clip_b.start_beat` (16.0) but not equal to it, so snapped-vs-raw are
    /// distinguishable (verified empirically: snap-on lands exactly at
    /// 16.0; Cmd-bypass lands at the raw ~16.0166, never exactly 16.0).
    #[test]
    fn move_drag_snaps_to_clip_edge_and_cmd_bypasses_it() {
        let mut panel = build_viewport();

        // Snap ON.
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        overlay.on_drag(body_pos_for(&panel, 17.9), &mut host, &mut ui_state, &mut panel);
        let snapped = host.find_clip_by_id("clip_a").unwrap().start_beat;
        assert_eq!(
            snapped,
            Beats::from_f32(16.0),
            "snap ON must land clip_a flush against clip_b's start edge"
        );

        // Same gesture, Cmd held: must NOT snap.
        let mut host2 = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state2 = UIState::new();
        ui_state2.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay2 = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay2.set_modifiers(Modifiers {
            command: true,
            ..Modifiers::NONE
        });
        overlay2.on_begin_drag(body_pos_for(&panel, 2.0), &mut host2, &mut ui_state2, &panel);
        overlay2.on_drag(body_pos_for(&panel, 17.9), &mut host2, &mut ui_state2, &mut panel);
        let raw = host2.find_clip_by_id("clip_a").unwrap().start_beat;
        assert_ne!(
            raw,
            Beats::from_f32(16.0),
            "Cmd held mid-drag must bypass snap entirely (raw position), got exactly the snapped edge"
        );
    }

    /// B12: a drag landing near a timeline marker snaps to it, exactly like
    /// a clip edge — same shared `magnetic_snap`, a marker candidate is just
    /// another entry it considers. Marker at beat 10.03 (deliberately off
    /// the 0.25 grid at this zoom, so a snap to 10.03 can only be the
    /// marker winning, not a coincidental grid line). Press `clip_a` at
    /// mouse-beat 2.0, land at mouse-beat 12.02 -> anchor 10.02, within
    /// range of the marker.
    #[test]
    fn move_drag_snaps_to_marker() {
        let mut panel = build_viewport();
        panel.set_markers(vec![crate::UiMarker::new(Beats::from_f32(10.03))]);

        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        overlay.on_drag(body_pos_for(&panel, 12.02), &mut host, &mut ui_state, &mut panel);

        let snapped = host.find_clip_by_id("clip_a").unwrap().start_beat;
        assert_eq!(
            snapped,
            Beats::from_f32(10.03),
            "a drag landing near a marker must snap to it"
        );
    }

    /// D15 landing-line flash, re-hooked at drag-end (2026-07-07): a move
    /// that actually lands somewhere new fires the flash with the landed
    /// beat + layer span; a gesture that never moved stays dark (the flash
    /// marks a landing, not a click).
    #[test]
    fn move_drag_fires_landing_flash_at_commit_only_when_landed() {
        let mut panel = build_viewport();
        let mut host = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state = UIState::new();
        ui_state.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);

        overlay.on_begin_drag(body_pos_for(&panel, 2.0), &mut host, &mut ui_state, &panel);
        overlay.on_drag(body_pos_for(&panel, 10.0), &mut host, &mut ui_state, &mut panel);
        overlay.on_end_drag(&mut host);

        let (_, beat, lo, hi) = overlay.landing_flash().expect("flash fires on a landed move");
        let landed = host.find_clip_by_id("clip_a").unwrap().start_beat;
        assert!(
            (beat - landed).abs() < Beats(1e-6),
            "flash beat {beat:?} must be the landed start beat {landed:?}"
        );
        assert_eq!((lo, hi), (0, 0), "single-layer drag spans exactly its layer");

        // Same gesture, zero displacement: begin then release in place.
        let mut host2 = GestureTestHost::new(&["layer-0"]).with_clip("clip_a", 0, 0.0, 8.0);
        let mut ui_state2 = UIState::new();
        ui_state2.select_clips(vec![ClipId::new("clip_a")]);
        let mut overlay2 = InteractionOverlay::new(crate::color::CLIP_VERTICAL_PAD);
        overlay2.on_begin_drag(body_pos_for(&panel, 2.0), &mut host2, &mut ui_state2, &panel);
        overlay2.on_end_drag(&mut host2);
        assert!(
            overlay2.landing_flash().is_none(),
            "no flash when the gesture landed nowhere new"
        );
    }
}

// ── P2 motion tests (`UI_CRAFT_AND_MOTION_PLAN.md` D15/D17) ────────────────
// `InteractionOverlay` has no existing test harness (every other method here
// takes `&mut dyn TimelineEditingHost`, which would need a full mock to
// exercise) — these drive the new drag-visual state machine directly via
// its private fields/`tick`, which take no host, rather than building one.
#[cfg(test)]
mod motion_tests {
    use super::*;

    fn overlay() -> InteractionOverlay {
        InteractionOverlay::new(6.0)
    }

    #[test]
    fn lift_and_ghost_target_zero_and_one_when_idle() {
        let mut ov = overlay();
        assert_eq!(ov.drag_mode, DragMode::None);
        ov.tick(16.0);
        assert_eq!(ov.lift_amount(), 0.0, "no lift while not dragging");
        assert_eq!(ov.ghost_alpha(), 1.0, "fully solid while not dragging");
        assert_eq!(ov.error_shake_offset_px(), 0.0, "no shake while idle");
        assert!(ov.landing_flash().is_none(), "no landing flash while idle");
    }

    #[test]
    fn move_drag_ramps_lift_up_and_settles_to_one() {
        let mut ov = overlay();
        ov.drag_mode = DragMode::Move;
        ov.drag_snapshot_clip_ids.insert(ClipId::new("clip-a"));

        ov.tick(16.0);
        assert!(ov.lift_amount() > 0.0, "lift starts rising the first tick of a move drag");
        assert!(ov.is_drag_visual_target(&ClipId::new("clip-a")));
        assert!(!ov.is_drag_visual_target(&ClipId::new("clip-b")));

        // Drive it to settle at the full MOTION_MED_MS duration.
        ov.tick(color::MOTION_MED_MS);
        assert_eq!(ov.lift_amount(), 1.0, "settles fully lifted while still dragging");
    }

    #[test]
    fn alt_duplicate_drag_dims_ghost_then_solidifies_on_release() {
        let mut ov = overlay();
        ov.drag_mode = DragMode::Move;
        ov.duplicate_on_release = true;
        ov.drag_snapshot_clip_ids.insert(ClipId::new("clip-a"));

        ov.tick(color::MOTION_MED_MS);
        assert!(
            ov.ghost_alpha() < 1.0 && ov.ghost_alpha() > 0.0,
            "alt-dragging dims toward the ghost target: {}",
            ov.ghost_alpha()
        );

        // Release: drag_mode drops, ghost eases back up ("solidifies").
        ov.drag_mode = DragMode::None;
        ov.duplicate_on_release = false;
        ov.tick(color::MOTION_MED_MS);
        assert_eq!(ov.ghost_alpha(), 1.0, "fully solid once settled post-release");
        // The clip stays a visual target until every tween has caught up —
        // by now they all have, so the memory is dropped.
        assert!(!ov.is_drag_visual_target(&ClipId::new("clip-a")));
    }

    #[test]
    fn error_shake_fires_and_decays_to_zero() {
        let mut ov = overlay();
        ov.error_shake.fire(240.0);
        ov.tick(1.0);
        assert!(ov.error_shake_offset_px().abs() <= 3.0001, "amplitude capped near 3px");

        // Run past the full duration — decays back to inert.
        ov.tick(240.0);
        assert_eq!(ov.error_shake_offset_px(), 0.0, "shake settles to zero once finished");
    }

    #[test]
    fn landing_flash_reports_geometry_while_active() {
        let mut ov = overlay();
        ov.landing_flash_beat = Beats(4.0);
        ov.landing_flash_layers = (1, 3);
        ov.landing_flash.fire(color::MOTION_MED_MS);

        let (progress, beat, min_layer, max_layer) = ov.landing_flash().expect("active");
        assert!((0.0..=1.0).contains(&progress));
        assert_eq!(beat, Beats(4.0));
        assert_eq!((min_layer, max_layer), (1, 3));

        ov.tick(color::MOTION_MED_MS * 2.0);
        assert!(ov.landing_flash().is_none(), "flash finishes and reports idle");
    }
}
