// Cursor management for timeline interaction feedback.
// 1:1 port of Unity Cursors.cs — maps procedural cursor types to winit CursorIcon values.
//
// Unity generates 24x24 procedural textures for each cursor shape.
// In Rust/winit we use the platform's built-in cursor icons instead,
// which gives native appearance and automatic HiDPI support.
//
// Cursor types (from Unity Cursors.cs):
// - Default: standard arrow (no special interaction)
// - ResizeHorizontal: double-headed arrow (inspector resize, trim handles)
// - ResizeVertical: double-headed arrow (video/timeline split handle)
// - Move: four-way cross (clip drag move)
// - Blocked: not-allowed (invalid cross-layer drag)

/// The cursor shapes used by the timeline UI.
/// Maps 1:1 to Unity's Cursors static methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineCursor {
    /// Standard arrow. Unity: `Cursors.SetDefault()`
    Default,
    /// ↔ horizontal resize. Unity: `Cursors.SetResizeHorizontal()`
    /// Used for: inspector width drag, trim handle hover
    ResizeHorizontal,
    /// ↕ vertical resize. Unity: implied by PanelResizeHandle
    /// Used for: video/timeline split handle
    ResizeVertical,
    /// ✥ four-way move. Unity: `Cursors.SetMove()`
    /// Used for: clip body drag
    Move,
    /// ⊘ not-allowed. Unity: `Cursors.SetBlocked()`
    /// Used for: invalid cross-layer drag (video↔generator)
    Blocked,
}

/// Tracks the current cursor state to avoid redundant winit calls.
/// Call `set()` when interaction state changes; call `apply()` once per frame
/// to push the cursor to the window.
pub struct CursorManager {
    current: TimelineCursor,
    pending: TimelineCursor,
}

impl Default for CursorManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorManager {
    pub fn new() -> Self {
        Self {
            current: TimelineCursor::Default,
            pending: TimelineCursor::Default,
        }
    }

    /// Request a cursor change. Doesn't take effect until `apply()`.
    /// From Unity: Cursors.SetMove(), Cursors.SetBlocked(), etc.
    pub fn set(&mut self, cursor: TimelineCursor) {
        self.pending = cursor;
    }

    /// Reset to default cursor. Unity: `Cursors.SetDefault()`
    pub fn set_default(&mut self) {
        self.pending = TimelineCursor::Default;
    }

    /// Returns true if the cursor changed since last apply.
    /// Call once per frame; if true, call `apply()` with the winit window.
    pub fn needs_update(&self) -> bool {
        self.current != self.pending
    }

    /// Get the pending cursor as a winit CursorIcon.
    /// Call this to get the value to pass to `window.set_cursor()`.
    pub fn pending_cursor_icon(&self) -> TimelineCursor {
        self.pending
    }

    /// Mark the pending cursor as applied.
    /// Call after `window.set_cursor()` succeeds.
    pub fn mark_applied(&mut self) {
        self.current = self.pending;
    }

    /// Get the current active cursor.
    pub fn current(&self) -> TimelineCursor {
        self.current
    }
}

impl TimelineCursor {
    /// Convert to winit CursorIcon string name.
    /// Used by app.rs to call `window.set_cursor(winit::window::CursorIcon::*)`.
    pub fn to_winit_cursor_icon(self) -> &'static str {
        match self {
            TimelineCursor::Default => "Default",
            TimelineCursor::ResizeHorizontal => "ColResize",
            TimelineCursor::ResizeVertical => "RowResize",
            TimelineCursor::Move => "Move",
            TimelineCursor::Blocked => "NotAllowed",
        }
    }
}
