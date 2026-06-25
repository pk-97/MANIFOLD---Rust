//! Single transparent overlay covering the entire tracks area.
//! Centralises all clip interaction (click, hover, drag, trim, box-select).
//!
//! Mechanical translation of Assets/Scripts/UI/Timeline/InteractionOverlay.cs.
//!
//! All interaction routing goes through this struct. The viewport panel becomes
//! purely rendering + coordinate conversion. The overlay calls through the
//! `TimelineEditingHost` trait for operations that need engine/editing access.

use manifold_foundation::{Beats, ClipId, Seconds};
use std::collections::HashSet;

use crate::clip_hit_tester::{ClipHitResult, ClipHitTester, HitRegion};
use crate::input::Modifiers;
use crate::node::Vec2;
use crate::panels::viewport::TimelineViewportPanel;
use crate::timeline_editing_host::{ClipRef, TimelineCursor, TimelineEditingHost};
use crate::ui_state::UIState;

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
    } else if ui_state.has_region() {
        let r = ui_state.get_region();
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

// ── DragMode ────────────────────────────────────────────────────
// Unity InteractionOverlay line 37.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragMode {
    None,
    Move,
    TrimLeft,
    TrimRight,
    RegionSelect,
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
            region_drag_start_beat: Beats::ZERO,
            region_drag_start_layer: 0,
            drag_start_beat: Beats::ZERO,
            drag_offset_beats: Beats::ZERO,
            trim_original_start_beat: Beats::ZERO,
            trim_original_duration_beats: Beats::ZERO,
            trim_original_in_point: Seconds::ZERO,
            trim_originals: Vec::with_capacity(8),
            modifiers: Modifiers::NONE,
        }
    }

    /// True while any drag is in progress. Unity: IsDragging property.
    pub fn is_dragging(&self) -> bool {
        self.drag_mode != DragMode::None
    }

    /// Current drag mode (read-only, for external queries like auto-scroll).
    pub fn drag_mode(&self) -> DragMode {
        self.drag_mode
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
    // MOVE-DRAG POLLING
    // Unity InteractionOverlay.PollMoveDrag (lines 116-124).
    // Called from app.rs frame loop every frame during move drag.
    // Keeps edge auto-scroll running when pointer delta is zero.
    // ────────────────────────────────────────────────────────────

    pub fn poll_move_drag(
        &mut self,
        mouse_screen_pos: Vec2,
        host: &mut dyn TimelineEditingHost,
        ui_state: &mut UIState,
        viewport: &TimelineViewportPanel,
    ) {
        if self.drag_mode != DragMode::Move || self.drag_anchor_clip_id.is_none() {
            return;
        }
        self.handle_move_drag(mouse_screen_pos, host, ui_state, viewport);
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
            // Unity line 207: Shift → extend region to clip end
            let clip = host.find_clip_by_id(&hit.clip_id);
            if let Some(c) = clip {
                select_region_to(c.end_beat, c.layer_index, ui_state, host);
            }
        } else if ctrl {
            // Unity lines 208-212: Ctrl → toggle multi-select + auto-compute region
            let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
            ui_state.toggle_clip_selection(hit.clip_id.clone(), lid);
            self.update_region_from_clip_selection(ui_state, host);
        } else {
            // Unity line 214: bare click → select single
            let lid = host.layer_id_at_index(hit.layer_index).unwrap_or_default();
            ui_state.select_clip(hit.clip_id.clone(), lid);
        }

        // Unity line 216: always notify host
        host.on_clip_selected(&hit.clip_id);
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
        self.drag_layer_blocked = false;

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
        viewport: &TimelineViewportPanel,
    ) {
        match self.drag_mode {
            DragMode::Move => {
                self.handle_move_drag(pos, host, ui_state, viewport);
            }
            DragMode::TrimLeft => {
                let beat = viewport.pixel_to_beat(pos.x);
                self.handle_trim_left_drag(beat, host, viewport);
            }
            DragMode::TrimRight => {
                let beat = viewport.pixel_to_beat(pos.x);
                self.handle_trim_right_drag(beat, host, viewport);
            }
            DragMode::RegionSelect => {
                self.update_region_drag(pos, ui_state, viewport, host);
            }
            DragMode::None => {}
        }
    }

    /// Port of Unity InteractionOverlay.OnEndDrag (lines 356-446).
    ///
    /// Takes no `UIState`: end-of-drag commits the engine batch and clears the
    /// overlay's own transient state — selection is untouched here.
    pub fn on_end_drag(
        &mut self,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        // Unity lines 358-363: region select → finalize
        if self.drag_mode == DragMode::RegionSelect {
            host.invalidate_all_layer_bitmaps();
            self.drag_mode = DragMode::None;
            return;
        }

        let ended_move = self.drag_mode == DragMode::Move;
        host.begin_command_batch();

        if self.drag_mode == DragMode::Move {
            // Unity lines 370-386: finalize move snap + record commands
            self.finalize_move_snap(host, viewport);

            for snapshot in &self.drag_snapshots {
                if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id) {
                    let start_changed =
                        (clip.start_beat - snapshot.start_beat).abs() >= Beats(0.0001);
                    let layer_changed = clip.layer_index != snapshot.layer_index;
                    if start_changed || layer_changed {
                        host.record_move(
                            &snapshot.clip_id,
                            snapshot.start_beat,
                            clip.start_beat,
                            snapshot.layer_index,
                            clip.layer_index,
                        );
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
            "Move clips"
        } else {
            "Trim clips"
        };
        host.commit_command_batch(desc);

        // Unity lines 423-427: clear drag state
        self.drag_mode = DragMode::None;
        self.drag_snapshots.clear();
        self.drag_snapshot_clip_ids.clear();
        self.drag_anchor_clip_id = None;
        self.trim_clip_id = None;
        self.trim_originals.clear();

        // Unity lines 444-445
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
        viewport: &TimelineViewportPanel,
    ) {
        if self.drag_anchor_clip_id.is_none() {
            return;
        }
        if self.drag_snapshots.is_empty() {
            self.capture_drag_selection(ui_state, host);
        }

        // Unity line 470: auto-scroll
        host.auto_scroll_for_drag(screen_pos);
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

        // Unity lines 522-534: magnetic snap + beat delta
        let anchor_start_beat = mouse_beat - self.drag_offset_beats;
        let snapped = viewport.magnetic_snap(
            anchor_start_beat,
            self.drag_start_layer_index,
            &self
                .drag_snapshot_clip_ids
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
        );
        let mut beat_delta = snapped - self.drag_start_beat;
        // Clamp: don't let the leftmost clip go below beat 0
        beat_delta = beat_delta.max(-self.drag_selection_min_start_beat);

        // Apply beat delta to all clips (direct mutation during drag — committed in OnEndDrag)
        // Unity line 533: movingClip.StartBeat = Max(0, snapshot.StartBeat + beatDelta)
        for snapshot in &self.drag_snapshots {
            let new_start = (snapshot.start_beat + beat_delta).max(Beats::ZERO);
            host.set_clip_start_beat(&snapshot.clip_id, new_start);
        }

        host.invalidate_all_layer_bitmaps();
    }

    /// Port of Unity InteractionOverlay.HandleTrimLeftDrag (lines 539-560).
    fn handle_trim_left_drag(
        &mut self,
        mouse_beat: Beats,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let trim_id = match &self.trim_clip_id {
            Some(id) => id.clone(),
            None => return,
        };

        let min_duration = Beats(0.25); // 1/16 note minimum (Unity line 544)
        let spb = host.get_seconds_per_beat() as f64;

        // Snap context comes from the grabbed clip; the resulting edge delta is
        // shared by every selected clip (each then clamps individually).
        let clip_layer = host.find_clip_by_id(&trim_id).map_or(0, |c| c.layer_index);
        let snapped =
            viewport.magnetic_snap(mouse_beat, clip_layer, std::slice::from_ref(&trim_id));
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
        mouse_beat: Beats,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        let trim_id = match &self.trim_clip_id {
            Some(id) => id.clone(),
            None => return,
        };

        let min_duration = Beats(0.25); // Unity line 566

        // Snap context from the grabbed clip; the edge delta fans over the
        // whole selection (each clip clamps individually).
        let clip_layer = host.find_clip_by_id(&trim_id).map_or(0, |c| c.layer_index);
        let snapped =
            viewport.magnetic_snap(mouse_beat, clip_layer, std::slice::from_ref(&trim_id));
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
        if ui_state.has_region() {
            let region = ui_state.get_region().clone();
            if let Some(clip) = host.find_clip_by_id(clip_id) {
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

    /// Port of Unity InteractionOverlay.FinalizeMoveSnap (lines 756-772).
    fn finalize_move_snap(
        &mut self,
        host: &mut dyn TimelineEditingHost,
        viewport: &TimelineViewportPanel,
    ) {
        if self.drag_snapshots.is_empty() || self.drag_anchor_clip_id.is_none() {
            return;
        }

        let anchor_id = self.drag_anchor_clip_id.as_ref().unwrap();
        // Unity line 760: uses dragAnchorClip.StartBeat — the clip's CURRENT position
        // (after being moved during drag), NOT the snapshot's original start beat.
        let anchor_start = host.find_clip_by_id(anchor_id).map(|c| c.start_beat);

        if let Some(anchor_start) = anchor_start {
            let snapped = viewport.magnetic_snap(
                anchor_start,
                self.drag_start_layer_index,
                &self
                    .drag_snapshot_clip_ids
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            let snap_delta = snapped - anchor_start;
            if snap_delta.abs() < Beats(0.0001) {
                return;
            }
            // Unity lines 764-768: apply snap delta to all clips
            for snapshot in &self.drag_snapshots {
                if let Some(clip) = host.find_clip_by_id(&snapshot.clip_id) {
                    host.set_clip_start_beat(
                        &snapshot.clip_id,
                        (clip.start_beat + snap_delta).max(Beats::ZERO),
                    );
                }
            }
            host.invalidate_all_layer_bitmaps();
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
        viewport: &TimelineViewportPanel,
        host: &dyn TimelineEditingHost,
    ) {
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

    /// Port of Unity InteractionOverlay.UpdateRegionFromClipSelection (lines 854-881).
    fn update_region_from_clip_selection(
        &self,
        ui_state: &mut UIState,
        host: &dyn TimelineEditingHost,
    ) {
        let selected_ids = ui_state.get_selected_clip_ids();
        if selected_ids.len() < 2 {
            ui_state.clear_region();
            return;
        }

        let mut min_beat = Beats(f64::MAX);
        let mut max_beat = Beats(-f64::MAX);
        let mut min_layer = usize::MAX;
        let mut max_layer = 0usize;

        for id in &selected_ids {
            if let Some(clip) = host.find_clip_by_id(id) {
                min_beat = min_beat.min(clip.start_beat);
                max_beat = max_beat.max(clip.end_beat);
                min_layer = min_layer.min(clip.layer_index);
                max_layer = max_layer.max(clip.layer_index);
            }
        }

        if min_beat < max_beat {
            ui_state.set_region_from_clip_bounds(
                min_beat,
                max_beat,
                min_layer as i32,
                max_layer as i32,
                host.layers(),
            );
        }
    }

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
