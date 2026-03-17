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

use std::collections::HashSet;
use manifold_core::selection::SelectionRegion;
use crate::node::Vec2;

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
    pub interior_clip_ids: Vec<String>,
    /// Number of split commands generated (stored in the host's command batch).
    pub split_count: usize,
}

/// Lightweight clip reference returned by `find_clip_by_id`.
/// Avoids passing mutable project references through the trait.
pub struct ClipRef {
    pub clip_id: String,
    pub start_beat: f32,
    pub duration_beats: f32,
    pub end_beat: f32,
    pub layer_index: usize,
    pub in_point: f32,
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

    // ── Coordinate conversion ───────────────────────────────────────
    // Note: most coordinate conversion is handled by the viewport panel
    // (pixel_to_beat, layer_at_y). These screen-position methods are
    // for operations that need the full screen→content transform.

    /// Convert a screen position to a beat value. Unity: ScreenPositionToBeat(Vector2).
    fn screen_position_to_beat(&self, pos: Vec2) -> f32;

    /// Resolve a screen position to a layer index. Unity: GetLayerIndexAtScreenPosition(Vector2).
    fn get_layer_index_at_position(&self, pos: Vec2) -> Option<usize>;

    /// Convert a beat to seconds. Unity: BeatToTime(float).
    fn beat_to_time(&self, beat: f32) -> f32;

    // ── Clip operations ─────────────────────────────────────────────

    /// Create a clip at the given beat and layer. Returns clip ID or None.
    /// Beat should be pre-snapped by the caller. `grid_step` is the current
    /// grid interval in beats (used as the new clip's default duration).
    /// Unity: CreateClipAtPosition(float, int).
    fn create_clip_at_position(&mut self, beat: f32, layer: usize, grid_step: f32) -> Option<String>;

    /// Move a clip to a different layer (cross-layer drag).
    /// Unity: MoveClipToLayer(TimelineClip, int).
    fn move_clip_to_layer(&mut self, clip_id: &str, target_layer: usize);

    // ── Selection & UI ──────────────────────────────────────────────

    /// Notify host that a clip was selected (updates inspector, bitmap visuals).
    /// Unity: OnClipSelected(TimelineClip).
    fn on_clip_selected(&mut self, clip_id: &str);

    /// Show clip context menu. Unity: OnClipRightClick(TimelineClip, Vector2).
    fn on_clip_right_click(&mut self, clip_id: &str, screen_pos: Vec2);

    /// Inspect a layer (shows layer inspector). Unity: InspectLayer(int).
    fn inspect_layer(&mut self, layer_index: usize);

    // NOTE: select_region_to removed from trait — the overlay implements
    // the full Unity EditingService.SelectRegionTo logic as a free function
    // in interaction_overlay.rs, where it has access to both UIState and host.

    // ── Auto-scroll ─────────────────────────────────────────────────

    /// Auto-scroll timeline when dragging near viewport edges.
    /// Unity: AutoScrollTimelineForDrag(Vector2).
    fn auto_scroll_for_drag(&mut self, screen_pos: Vec2);

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
    fn scrub_to_time(&mut self, time: f32);

    // ── Overlap enforcement ─────────────────────────────────────────

    /// DaVinci-style overlap enforcement on a placed clip.
    /// Commands are collected internally by the host (added to the current batch).
    /// Unity: EnforceNonOverlap(TimelineClip, HashSet<string>).
    fn enforce_non_overlap(&mut self, clip_id: &str, ignore_ids: &HashSet<String>);

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
        old_start: f32, new_start: f32,
        old_layer: usize, new_layer: usize,
    );

    /// Record a trim operation into the current batch.
    fn record_trim(
        &mut self,
        clip_id: &str,
        old_start: f32, new_start: f32,
        old_duration: f32, new_duration: f32,
        old_in_point: f32, new_in_point: f32,
    );

    /// Commit the current command batch as a single undo entry.
    fn commit_command_batch(&mut self, description: &str);

    // ── Live clip mutation (during drag — committed to undo on EndDrag) ──

    /// Set a clip's start beat. Used during move drag to update position live.
    /// Unity: movingClip.StartBeat = ... (InteractionOverlay line 533).
    fn set_clip_start_beat(&mut self, clip_id: &str, beat: f32);

    /// Set a clip's trim state. Used during trim drag to update live.
    /// Unity: trimClip.StartBeat/DurationBeats/InPoint = ... (lines 554-557).
    fn set_clip_trim(&mut self, clip_id: &str, start_beat: f32, duration_beats: f32, in_point: f32);

    // ── Video metadata ──────────────────────────────────────────────

    /// Maximum clip duration in beats based on video source length and InPoint.
    /// Returns 0 if unavailable. Unity: GetMaxDurationBeats (InteractionOverlay line 960-971).
    fn get_max_duration_beats(&self, clip_id: &str) -> f32;
}
