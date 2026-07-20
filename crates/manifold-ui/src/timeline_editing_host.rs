//! Trait consumed by InteractionOverlay to abstract away engine/editing coupling.
//! Mechanical translation of Assets/Scripts/UI/Timeline/ITimelineEditingHost.cs.
//!
//! The app layer implements this trait, wrapping engine, editing service,
//! and UI state. The overlay calls through the trait during click, drag,
//! and trim operations — it never touches the engine directly.
//!
//! Commands are assembled by the host, not the overlay. The overlay calls
//! `begin_command_batch()`, records individual mutations via `record_move()`
//! / `record_trim()`, then `commit_command_batch()`. The host builds the
//! CompositeCommand internally. This avoids a dependency from manifold-ui
//! on manifold-editing.

use crate::node::Vec2;
use crate::view::{SelectionRegion, UiGraphTarget, UiLayer, UiSegmentShape};
use manifold_foundation::{Beats, ClipId, LayerId, ParamId, Seconds};
use std::collections::HashSet;

/// Cursor shapes the overlay can request.
/// Matches Unity Cursors.cs static methods: SetDefault, SetMove, SetResizeHorizontal, SetBlocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineCursor {
    Default,
    Move,
    ResizeHorizontal,
    Blocked,
}

/// Result from splitting clips at region boundaries for move-partial drag.
/// Matches Unity EditingService.RegionSplitResult.
pub struct RegionSplitResult {
    /// Clip IDs of the interior segments (the clips being dragged).
    pub interior_clip_ids: Vec<ClipId>,
    /// Number of split commands generated (stored in the host's command batch).
    pub split_count: usize,
}

/// Lightweight clip reference returned by `find_clip_by_id`.
/// Avoids passing mutable project references through the trait.
pub struct ClipRef {
    pub clip_id: ClipId,
    pub start_beat: Beats,
    pub duration_beats: Beats,
    pub end_beat: Beats,
    pub layer_index: usize,
    pub layer_id: LayerId,
    pub in_point: Seconds,
    pub is_generator: bool,
    pub is_locked: bool,
    pub is_looping: bool,
}

/// Abstraction consumed by InteractionOverlay.
/// Port of ITimelineEditingHost.cs — every method maps 1:1 to a Unity interface method.
///
/// The app layer implements this trait, wrapping engine, editing service,
/// UI root, and selection state.
pub trait TimelineEditingHost {
    // ── Data access (Unity: CurrentProject, IsPlaying) ──────────────

    /// Number of layers in the active timeline.
    fn layer_count(&self) -> usize;

    /// Read-only access to the layer array (for populating region selection LayerIds).
    fn layers(&self) -> &[UiLayer];

    /// Get the LayerId at a positional index (for resolving indices to stable IDs).
    fn layer_id_at_index(&self, index: usize) -> Option<LayerId>;

    /// Whether a layer is a generator layer (for cross-layer type compatibility).
    fn layer_is_generator(&self, index: usize) -> bool;

    /// Whether a layer is muted. Unity: IsLayerMuted(int).
    fn is_layer_muted(&self, index: usize) -> bool;

    /// Beats per bar from project settings.
    fn project_beats_per_bar(&self) -> u32;

    /// Current seconds per beat. Unity: GetSecondsPerBeat().
    fn get_seconds_per_beat(&self) -> f32;

    /// Whether playback is active. Unity: IsPlaying.
    fn is_playing(&self) -> bool;

    // ── Clip queries ────────────────────────────────────────────────

    /// Find a clip by ID. Unity: FindClipById(string).
    fn find_clip_by_id(&self, clip_id: &str) -> Option<ClipRef>;

    /// All clips on a given layer, for whole-clip range selection (D2's
    /// shift-click-on-clip gesture, `docs/TIMELINE_INTERACTION_P1_SPEC.md`).
    /// Empty if the layer index is out of range. Order is not guaranteed —
    /// callers that need contiguous-range semantics filter/sort by
    /// `start_beat` themselves.
    fn clips_on_layer(&self, layer_index: usize) -> Vec<ClipRef>;

    // ── Coordinate conversion ───────────────────────────────────────
    // Note: most coordinate conversion is handled by the viewport panel
    // (pixel_to_beat, layer_at_y). These screen-position methods are
    // for operations that need the full screen→content transform.

    /// Convert a screen position to a beat value. Unity: ScreenPositionToBeat(Vector2).
    fn screen_position_to_beat(&self, pos: Vec2) -> Beats;

    /// Resolve a screen position to a layer index. Unity: GetLayerIndexAtScreenPosition(Vector2).
    fn get_layer_index_at_position(&self, pos: Vec2) -> Option<usize>;

    /// Convert a beat to seconds. Unity: BeatToTime(float).
    fn beat_to_time(&self, beat: Beats) -> Seconds;

    // ── Clip operations ─────────────────────────────────────────────

    /// Create a clip at the given beat and layer. Returns clip ID or None.
    /// Beat should be pre-snapped by the caller. `grid_step` is the current
    /// grid interval in beats (used as the new clip's default duration).
    /// Unity: CreateClipAtPosition(float, int).
    fn create_clip_at_position(
        &mut self,
        beat: Beats,
        layer: usize,
        grid_step: Beats,
    ) -> Option<ClipId>;

    /// Move a clip to a different layer (cross-layer drag).
    /// Unity: MoveClipToLayer(TimelineClip, int).
    fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize);

    // ── Selection & UI ──────────────────────────────────────────────

    /// Notify host that a clip was selected (updates inspector, bitmap visuals).
    /// Unity: OnClipSelected(TimelineClip).
    fn on_clip_selected(&mut self, clip_id: &str);

    /// Show clip context menu. Unity: OnClipRightClick(TimelineClip, Vector2).
    fn on_clip_right_click(&mut self, clip_id: &str, screen_pos: Vec2);

    /// Show track/layer context menu on empty area right-click.
    /// Unity: InputHandler.HandleEmptyAreaRightClick → ShowLayerContextMenu.
    fn on_track_right_click(&mut self, beat: Beats, layer_index: usize, screen_pos: Vec2);

    /// Show an automation-lane context menu. Fired on right-click
    /// anywhere on a lane's strip/segment/dot — `on_pointer_click` resolves
    /// which lane was hit before calling this, same as `on_track_right_click`
    /// resolves the layer index.
    fn on_automation_lane_right_click(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        screen_pos: Vec2,
    );

    /// Inspect a layer (shows layer inspector). Unity: InspectLayer(int).
    fn inspect_layer(&mut self, layer_index: usize);

    // NOTE: select_region_to removed from trait — the overlay implements
    // the full Unity EditingService.SelectRegionTo logic as a free function
    // in interaction_overlay.rs, where it has access to both UIState and host.

    // NOTE: auto_scroll_for_drag (Unity: AutoScrollTimelineForDrag) removed
    // from the trait — it was a permanent no-op stub ("the actual scroll
    // logic remains in tick_and_render", which never materialized). B11
    // (`TIMELINE_INTERACTION_P1_SPEC.md`) implements edge autoscroll directly
    // on `TimelineViewportPanel::autoscroll_edge` (the single P0 scroll
    // owner), called from the overlay's drag handlers — no host indirection
    // needed since the viewport already owns the scroll state.

    // ── Bitmap invalidation ─────────────────────────────────────────

    /// Invalidate a specific layer's bitmap. Unity: InvalidateLayerBitmap(int).
    fn invalidate_layer_bitmap(&mut self, layer_index: usize);

    /// Invalidate all layer bitmaps. Unity: InvalidateAllLayerBitmaps().
    fn invalidate_all_layer_bitmaps(&mut self);

    /// Mark the timeline UI as needing a visual refresh. Unity: MarkDirty().
    fn mark_dirty(&mut self);

    // ── Cursor ──────────────────────────────────────────────────────

    /// Set the cursor shape. Matches Unity Cursors.SetDefault/SetMove/SetResizeHorizontal/SetBlocked.
    fn set_cursor(&mut self, cursor: TimelineCursor);

    // ── Playback ────────────────────────────────────────────────────

    /// Scrub the playhead to a time in seconds. Unity: ScrubToTime(float).
    fn scrub_to_time(&mut self, time: Seconds);

    // ── Overlap enforcement ─────────────────────────────────────────

    /// DaVinci-style overlap enforcement on a placed clip.
    /// Commands are collected internally by the host (added to the current batch).
    /// Unity: EnforceNonOverlap(TimelineClip, HashSet<string>).
    fn enforce_non_overlap(&mut self, clip_id: &str, ignore_ids: &HashSet<ClipId>);

    // ── Region-partial move ─────────────────────────────────────────

    /// Split clips at region boundaries for move-partial drag.
    /// Split commands are stored internally by the host (prepended to undo batch).
    /// Unity: SplitClipsForRegionMove(SelectionRegion).
    fn split_clips_for_region_move(&mut self, region: &SelectionRegion) -> RegionSplitResult;

    // ── Command batching ────────────────────────────────────────────
    // The overlay tells the host WHAT changed; the host handles undo internally.
    // This avoids manifold-ui depending on manifold-editing.

    /// Begin a command batch. All record_* calls until commit are grouped.
    fn begin_command_batch(&mut self);

    /// Record a move operation into the current batch.
    fn record_move(
        &mut self,
        clip_id: &str,
        old_start: Beats,
        new_start: Beats,
        old_layer: usize,
        new_layer: usize,
    );

    /// Record a trim operation into the current batch.
    fn record_trim(
        &mut self,
        clip_id: &str,
        old_start: Beats,
        new_start: Beats,
        old_duration: Beats,
        new_duration: Beats,
        old_in_point: Seconds,
        new_in_point: Seconds,
    );

    /// Drop a copy of `src_clip_id` at `target_beat` on `target_layer` into the
    /// CURRENT command batch (committed with the move on commit_command_batch),
    /// so an opt/alt-drag duplicate is one undo entry alongside the move.
    fn duplicate_clip_to(&mut self, src_clip_id: &str, target_beat: Beats, target_layer: usize);

    /// Commit the current command batch as a single undo entry.
    fn commit_command_batch(&mut self, description: &str);

    // ── Live clip mutation (during drag — committed to undo on EndDrag) ──

    /// Set a clip's start beat. Used during move drag to update position live.
    /// Unity: movingClip.StartBeat = ... (InteractionOverlay line 533).
    fn set_clip_start_beat(&mut self, clip_id: &str, beat: Beats);

    /// Set a clip's trim state. Used during trim drag to update live.
    /// Unity: trimClip.StartBeat/DurationBeats/InPoint = ... (lines 554-557).
    fn set_clip_trim(
        &mut self,
        clip_id: &str,
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
    );

    // ── Video metadata ──────────────────────────────────────────────

    /// Maximum clip duration in beats based on video source length and InPoint.
    /// Returns 0 if unavailable. Unity: GetMaxDurationBeats (InteractionOverlay line 960-971).
    fn get_max_duration_beats(&self, clip_id: &str) -> Beats;

    // ── Automation lane editing (P4, `docs/AUTOMATION_LANES_DESIGN.md` §7) ──
    //
    // Mirrors the clip-drag shape above: a single click/double-click action
    // executes + sends immediately (like `create_clip_at_position`); a drag
    // mutates the point live each frame via `set_automation_point_preview`
    // (like `set_clip_start_beat` — bypasses undo, "already applied" by the
    // time the commit method runs), then commits ONE undo entry on release.
    // Values are always PARAM-RANGE `f32` (never normalized) — the overlay
    // denormalizes via `AutomationLaneScreen::param_min/max` /
    // `UiAutomationLane::denormalize` before calling any of these, so the
    // host never needs registry access to resolve a range.

    /// Add a breakpoint to `param_id`'s lane on `target` at `beat` (already
    /// grid-snapped by the caller unless Cmd was held) with `value` in PARAM
    /// RANGE and `shape` the segment leaving the new point. Creates the lane
    /// if none exists yet. Executes + sends immediately — no batch.
    fn add_automation_point(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        beat: Beats,
        value: f32,
        shape: UiSegmentShape,
    );

    /// Live-preview a point drag: directly mutates the point currently at
    /// `from_beat` to `(to_beat, to_value)` in PARAM RANGE, bypassing undo.
    /// The caller re-derives `from_beat` each frame as whatever beat this
    /// method itself last wrote (starting from the grabbed point's original
    /// beat) — see `InteractionOverlay`'s automation drag state, mirroring
    /// how clip move-drag always recomputes from the drag-start snapshot
    /// rather than incrementally.
    fn set_automation_point_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        from_beat: Beats,
        to_beat: Beats,
        to_value: f32,
    );

    /// Commit a completed point drag as one undo entry. `old` is the point's
    /// state BEFORE the drag started (the explicit reverse, captured at grab
    /// time — the `MoveAutomationPointCommand` drag-commit precedent); `new`
    /// is its final state. The point is already at `new` in the live project
    /// (from `set_automation_point_preview` calls during the drag) — this
    /// only registers the undo entry and mirrors it to the content thread,
    /// same as `record_move` + `commit_command_batch`'s "already applied"
    /// comment.
    fn commit_automation_point_move(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        old: (Beats, f32, UiSegmentShape),
        new: (Beats, f32, UiSegmentShape),
    );

    /// Remove the breakpoint at `beat` (double-click or Delete key). Looks up
    /// the point's current index within the lane at call time. No-op if no
    /// point exists at that beat. Executes + sends immediately.
    fn remove_automation_point(&mut self, target: &UiGraphTarget, param_id: &ParamId, beat: Beats);

    // ── Automation lane editing — segment gestures (P4 Unit B,
    // `docs/AUTOMATION_LANES_DESIGN.md` §7's "drag a segment" / "modifier-drag
    // a segment") ────────────────────────────────────────────────────────

    /// Live-preview an Alt-drag curve bend: directly sets the point at
    /// `left_beat`'s `shape` to `Curved(bend)`, bypassing undo — the shape-only
    /// twin of `set_automation_point_preview` (beat/value are untouched by
    /// this gesture, so there's no `from`/`to` beat to re-derive). Commit
    /// reuses `commit_automation_point_move` directly: old/new share
    /// beat+value and differ only in `shape`.
    fn set_automation_segment_bend_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left_beat: Beats,
        bend: f32,
    );

    /// Live-preview a vertical segment drag: both endpoints move by the same
    /// value delta (already computed by the caller — this just writes the two
    /// resulting PARAM-RANGE values), bypassing undo. Beats are unchanged.
    fn set_automation_segment_drag_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left_beat: Beats,
        left_value: f32,
        right_beat: Beats,
        right_value: f32,
    );

    /// Commit a completed vertical segment drag as ONE undo entry covering
    /// both endpoints. Each tuple is `(beat, old_value, new_value, shape)` —
    /// `shape` is unchanged by this gesture, carried through so the resulting
    /// commands preserve it exactly. Already applied live by
    /// `set_automation_segment_drag_preview`; this only registers the undo
    /// entry (mirrors `commit_automation_point_move`'s "already applied"
    /// shape, batched over two points instead of one).
    fn commit_automation_segment_drag(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        left: (Beats, f32, f32, UiSegmentShape),
        right: (Beats, f32, f32, UiSegmentShape),
    );

    // ── Automation lane editing — marquee group move (P4 Unit B) ─────

    /// Commit a marquee group-move as ONE undo entry. Each tuple is
    /// `(target, param_id, beat, old_value, new_value, shape)` — beat/shape
    /// unchanged by this gesture. Already applied live (per-point, via
    /// repeated `set_automation_point_preview` calls with `from_beat ==
    /// to_beat`) — this only registers the batched undo entry.
    fn commit_automation_group_move(
        &mut self,
        moves: Vec<(UiGraphTarget, ParamId, Beats, f32, f32, UiSegmentShape)>,
    );

    // ── Automation lane editing — draw/pencil mode (P4 Unit B, §7's
    // "Draw mode") ────────────────────────────────────────────────────

    /// Full (UNFILTERED by visible beat range) point list for `target`/
    /// `param_id`'s lane, as `(beat, value, shape)` triples in PARAM-RANGE
    /// units. `None` when no lane exists yet for this param. Needed at
    /// draw-stroke grab time — `AutomationLaneScreen::dots` is culled to the
    /// visible range and would silently drop off-screen points from the
    /// stroke's eventual install.
    fn automation_lane_points(
        &self,
        target: &UiGraphTarget,
        param_id: &ParamId,
    ) -> Option<Vec<(Beats, f32, UiSegmentShape)>>;

    /// Live-preview an in-progress draw stroke: overwrites the WHOLE lane's
    /// point list, bypassing undo (creates the lane, enabled, if it doesn't
    /// exist yet — same as a click-add's implicit lane creation).
    fn set_automation_draw_preview(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        points: Vec<(Beats, f32, UiSegmentShape)>,
    );

    /// Commit a finished draw stroke as ONE undo entry — installs
    /// `new_points` via the same mechanism §5's Automation Arm recording
    /// uses (`CommitRecordedGestureCommand`): `old_points` is the pre-stroke
    /// set (`None` if the stroke created the lane, mirroring
    /// `AddAutomationPointCommand`'s `created_lane` semantics).
    fn commit_automation_draw_stroke(
        &mut self,
        target: &UiGraphTarget,
        param_id: &ParamId,
        new_points: Vec<(Beats, f32, UiSegmentShape)>,
        old_points: Option<Vec<(Beats, f32, UiSegmentShape)>>,
    );
}
