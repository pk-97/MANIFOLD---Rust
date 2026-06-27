//! Viewport-local interaction: clip/marker hit-testing, hover, and the ruler /
//! overview / marker-drag event routing. (Clip move/trim/region lives in
//! `interaction_overlay.rs`.) See `docs/TIMELINE_API_DESIGN.md` §3.6.

use super::*;
use crate::clip_hit_tester::ClipHitTester;

impl TimelineViewportPanel {
    /// Hit-test a point against marker flags in the ruler area.
    /// Returns the MarkerId if a flag was hit.
    ///
    /// Recomputes each flag rect from the marker model — the same geometry the
    /// flag node is drawn at — so there is no parallel rect list to keep in
    /// sync and the clickable area can never go stale after a scroll update.
    pub fn hit_test_marker_flag(&self, pos: Vec2) -> Option<MarkerId> {
        self.markers
            .iter()
            .find(|m| self.marker_flag_rect(m.beat).contains(pos))
            .map(|m| m.id.clone())
    }

    /// Hit-test a screen position against all clips.
    /// Returns the topmost clip hit and which region was hit (body, trim left, trim right).
    ///
    /// Delegates to the single canonical [`ClipHitTester::hit_test`] — the same
    /// hit-tester the click/drag path uses (`InteractionOverlay::hit_test_at`) — so
    /// hover and click agree on trim zones *and* group-layer skipping. This used to
    /// be a divergent copy with fixed-width trim handles and no group skip, which
    /// meant a clip edge could hover-as-body but grab-as-trim. Coordinate handling
    /// mirrors `hit_test_at`: pointer Y is converted into scroll-adjusted
    /// track-content space.
    pub fn hit_test_clip(&self, pos: Vec2) -> Option<ClipHitResult> {
        if !self.tracks_rect.contains(pos) {
            return None;
        }

        let beat = self.pixel_to_beat(pos.x).as_f32();
        let y_in_track_content = (pos.y - self.tracks_rect.y) + self.scroll_y_px();

        ClipHitTester::hit_test(
            beat,
            y_in_track_content,
            CLIP_VERTICAL_PAD,
            &self.mapper,
            |i| self.clips_for_layer(i),
            |i| self.is_group_layer(i),
        )
    }

    /// Called every frame (or on CursorMoved) with the current cursor position
    /// to update clip hover state. Matches Unity's OnPointerMove continuous hit-testing.
    pub fn update_hover_at(&mut self, pos: Vec2) -> Vec<PanelAction> {
        if !self.tracks_rect.contains(pos) {
            if self.hovered_clip_id.is_some() {
                self.hovered_clip_id = None;
                return vec![PanelAction::ViewportHoverChanged(None)];
            }
            return Vec::new();
        }

        let new_hover = self.hit_test_clip(pos).map(|h| h.clip_id);
        if new_hover != self.hovered_clip_id {
            self.hovered_clip_id = new_hover.clone();
            return vec![PanelAction::ViewportHoverChanged(new_hover)];
        }
        Vec::new()
    }

    /// Resolve a press in the horizontal scrollbar strip to a pan action, latching
    /// the grab offset for the subsequent drag (§24 5e). A press on the thumb grabs
    /// it where touched; a press on the track centres the thumb under the pointer.
    fn scrollbar_h_press(&mut self, pos: Vec2) -> Option<PanelAction> {
        let (_, thumb) = self.scrollbar_h_layout()?;
        self.scrollbar_grab_dx = if thumb.contains(pos) {
            pos.x - thumb.x
        } else {
            thumb.width * 0.5
        };
        let thumb_left = pos.x - self.scrollbar_grab_dx;
        let sx = self.scrollbar_h_scroll_at(thumb_left)?;
        Some(PanelAction::TimelineScrollbarH(sx))
    }

    /// Route a viewport-local pointer event (the `Panel::handle_event` body).
    ///
    /// Only ruler/overview/marker interaction lives here — tracks-area clip
    /// click/drag/hover is owned by `InteractionOverlay` in app.rs.
    pub(super) fn on_timeline_event(&mut self, event: &UIEvent) -> Vec<PanelAction> {
        match event {
            // ── Click: marker flag → ruler → overview strip ───────
            UIEvent::Click { pos, modifiers, .. } => {
                // Marker flag hit-test (priority over ruler scrub)
                if let Some(marker_id) = self.hit_test_marker_flag(*pos) {
                    return vec![PanelAction::MarkerClicked(
                        marker_id.to_string(),
                        *modifiers,
                    )];
                }
                if self.overview_rect.contains(*pos) {
                    let norm =
                        ((pos.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.ruler_rect.contains(*pos) {
                    let raw = self.pixel_to_beat(pos.x);
                    let beat = self.scrub_snap_beat(raw, modifiers.alt);
                    return vec![PanelAction::Seek(beat.as_f32())];
                }
                // Horizontal scrollbar: click the track to jump (centre the thumb
                // under the pointer), or click the thumb to no-op-then-drag.
                if self.scrollbar_h_rect.contains(*pos)
                    && let Some(action) = self.scrollbar_h_press(*pos)
                {
                    return vec![action];
                }
                Vec::new()
            }

            // ── DragBegin: marker flag → ruler → overview scrub ──
            UIEvent::DragBegin {
                origin, modifiers, ..
            } => {
                // Marker flag drag (priority over ruler scrub)
                if let Some(marker_id) = self.hit_test_marker_flag(*origin) {
                    self.drag_mode = ViewportDragMode::MarkerDrag;
                    // Store start beat for undo
                    self.marker_drag_start_beat = self
                        .markers
                        .iter()
                        .find(|m| m.id == marker_id)
                        .map(|m| m.beat)
                        .unwrap_or(Beats::ZERO);
                    self.marker_drag_id = Some(marker_id.clone());
                    return vec![PanelAction::MarkerDragStarted(marker_id.to_string())];
                }
                if self.overview_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::OverviewScrub;
                    self.scrub_free = false;
                    let norm = ((origin.x - self.overview_rect.x) / self.overview_rect.width)
                        .clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.ruler_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::RulerScrub;
                    // Latch Alt state at drag start — Unity checks per-frame but
                    // Drag events don't carry modifiers, so we capture once.
                    self.scrub_free = modifiers.alt;
                    let raw = self.pixel_to_beat(origin.x);
                    let beat = self.scrub_snap_beat(raw, self.scrub_free);
                    return vec![PanelAction::Seek(beat.as_f32())];
                }
                // Horizontal scrollbar drag (§24 5e). Latches the grab offset so
                // the thumb tracks the pointer 1:1.
                if self.scrollbar_h_rect.contains(*origin) {
                    self.drag_mode = ViewportDragMode::ScrollbarHDrag;
                    if let Some(action) = self.scrollbar_h_press(*origin) {
                        return vec![action];
                    }
                    return Vec::new();
                }
                Vec::new()
            }

            // ── Drag: marker → ruler → overview scrub continuation
            UIEvent::Drag { pos, .. } => {
                if self.drag_mode == ViewportDragMode::MarkerDrag
                    && let Some(marker_id) = &self.marker_drag_id
                {
                    let raw = self.pixel_to_beat(pos.x);
                    let beat = self.scrub_snap_beat(raw, false).max(Beats::ZERO);
                    return vec![PanelAction::MarkerDragMoved(
                        marker_id.to_string(),
                        beat.as_f32(),
                    )];
                }
                if self.drag_mode == ViewportDragMode::OverviewScrub {
                    let norm =
                        ((pos.x - self.overview_rect.x) / self.overview_rect.width).clamp(0.0, 1.0);
                    return vec![PanelAction::OverviewScrub(norm)];
                }
                if self.drag_mode == ViewportDragMode::RulerScrub {
                    // Clamp pixel to ruler rect so dragging outside the viewport
                    // doesn't seek to extreme positions.
                    let clamped_x = pos
                        .x
                        .clamp(self.ruler_rect.x, self.ruler_rect.x + self.ruler_rect.width);
                    let raw = self.pixel_to_beat(clamped_x);
                    let beat = self.scrub_snap_beat(raw, self.scrub_free);
                    return vec![PanelAction::Seek(beat.as_f32())];
                }
                if self.drag_mode == ViewportDragMode::ScrollbarHDrag {
                    let thumb_left = pos.x - self.scrollbar_grab_dx;
                    if let Some(sx) = self.scrollbar_h_scroll_at(thumb_left) {
                        return vec![PanelAction::TimelineScrollbarH(sx)];
                    }
                    return Vec::new();
                }
                Vec::new()
            }

            // ── DragEnd: reset drag mode ─────────────────────────
            UIEvent::DragEnd { pos, .. } => {
                if self.drag_mode == ViewportDragMode::MarkerDrag {
                    let result = if let Some(marker_id) = self.marker_drag_id.take() {
                        let raw = self.pixel_to_beat(pos.x);
                        let beat = self.scrub_snap_beat(raw, false).max(Beats::ZERO);
                        vec![PanelAction::MarkerDragEnded(
                            marker_id.to_string(),
                            beat.as_f32(),
                        )]
                    } else {
                        Vec::new()
                    };
                    self.drag_mode = ViewportDragMode::None;
                    return result;
                }
                if self.drag_mode != ViewportDragMode::None {
                    self.drag_mode = ViewportDragMode::None;
                    self.scrub_free = false;
                }
                Vec::new()
            }

            // ── DoubleClick: marker rename ────────────────────────
            UIEvent::DoubleClick { pos, .. } => {
                if let Some(marker_id) = self.hit_test_marker_flag(*pos) {
                    return vec![PanelAction::MarkerDoubleClicked(marker_id.to_string())];
                }
                Vec::new()
            }

            // ── RightClick: marker context menu ──────────────────
            UIEvent::RightClick { pos, .. } => {
                if let Some(marker_id) = self.hit_test_marker_flag(*pos) {
                    return vec![PanelAction::MarkerRightClicked(marker_id.to_string())];
                }
                Vec::new()
            }

            // All other events handled by InteractionOverlay — return empty.
            _ => Vec::new(),
        }
    }
}
