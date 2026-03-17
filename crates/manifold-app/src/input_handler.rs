//! Keyboard dispatch, zoom logic, and context menu routing.
//! Mechanical translation of Assets/Scripts/UI/Timeline/InputHandler.cs.
//!
//! Plain Rust struct — NOT a MonoBehaviour equivalent.
//! Returns Vec<PanelAction> for most shortcuts; the app layer dispatches them.
//! Zoom state (pending anchors, scroll targets) is owned here.

use manifold_ui::input::Modifiers;
use manifold_ui::panels::PanelAction;

/// Keyboard/zoom handler. Port of InputHandler.cs.
///
/// Owns zoom state (pending anchor, scroll target) and inspector focus.
/// The app layer calls `handle_keyboard_input()` on each key press and
/// dispatches the returned PanelActions.
pub struct InputHandler {
    // ── Zoom state (Unity lines 51-59) ──
    pub needs_zoom_update: bool,
    pub has_pending_zoom_anchor: bool,
    pub pending_zoom_anchor_beat: f32,
    pub pending_zoom_anchor_viewport_x: f32,
    pub pending_zoom_scroll_time: f32, // -1.0 = no pending

    // ── Panel focus (Unity line 65) ──
    pub inspector_has_focus: bool,
}

impl InputHandler {
    pub fn new() -> Self {
        Self {
            needs_zoom_update: false,
            has_pending_zoom_anchor: false,
            pending_zoom_anchor_beat: 0.0,
            pending_zoom_anchor_viewport_x: 0.0,
            pending_zoom_scroll_time: -1.0,
            inspector_has_focus: false,
        }
    }

    pub fn set_inspector_focus(&mut self, focused: bool) {
        self.inspector_has_focus = focused;
    }

    pub fn clear_needs_zoom_update(&mut self) {
        self.needs_zoom_update = false;
    }

    pub fn clear_pending_zoom_anchor(&mut self) {
        self.has_pending_zoom_anchor = false;
    }

    pub fn clear_pending_zoom_scroll_time(&mut self) {
        self.pending_zoom_scroll_time = -1.0;
    }

    // NOTE: The full handle_keyboard_input() method will be wired in Step 6
    // when we move the keyboard match block from app.rs to here.
    // For now, this struct holds the zoom and focus state that the
    // keyboard handler needs, and app.rs continues to dispatch directly.
    //
    // The move is deferred to Step 6 (wire + delete old code) because:
    // 1. The keyboard block in app.rs directly calls self.engine, self.editing_service,
    //    self.selection — these need to be accessed through PanelActions or a host trait
    // 2. Moving 600 lines of match arms requires updating every reference simultaneously
    // 3. It's safer to move in one atomic step alongside wiring the other new structs

    // ── Zoom (Unity InputHandler lines 864-1006) ─────────────────

    /// Queue a zoom anchor at the playhead position.
    /// After zoom + rebuild, scroll will be adjusted to keep this beat
    /// at the same viewport X position.
    /// Port of Unity InputHandler.QueuePlayheadZoomAnchor (lines 959-966).
    pub fn queue_playhead_zoom_anchor(&mut self, playhead_beat: f32, playhead_viewport_x: f32) {
        self.pending_zoom_scroll_time = -1.0;
        self.has_pending_zoom_anchor = true;
        self.pending_zoom_anchor_beat = playhead_beat;
        self.pending_zoom_anchor_viewport_x = playhead_viewport_x;
    }

    /// Apply pending zoom scroll after a rebuild or zoom update.
    /// Port of Unity InputHandler.ApplyPendingZoomScroll (lines 1013-1024).
    /// Returns true if scroll was applied (caller should trigger rebuild).
    pub fn apply_pending_zoom_scroll(&mut self) -> bool {
        if self.has_pending_zoom_anchor {
            self.has_pending_zoom_anchor = false;
            // Caller uses pending_zoom_anchor_beat + viewport_x to call
            // scroll_to_keep_beat_at_viewport_x() on the viewport
            return true;
        }
        if self.pending_zoom_scroll_time >= 0.0 {
            // Caller uses pending_zoom_scroll_time to call scroll_to_center()
            let _time = self.pending_zoom_scroll_time;
            self.pending_zoom_scroll_time = -1.0;
            return true;
        }
        false
    }
}
