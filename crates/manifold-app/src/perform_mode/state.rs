//! Performance mode state.
//!
//! All state required to drive the perform-mode HUD lives here, in one
//! struct. The `Application` holds a single `PerformModeState` field
//! instead of scattered booleans, so adding new HUD data later is a
//! single-struct edit instead of a hunt through `Application`.

use manifold_ui::node::Rect;

/// State for performance mode. Owned by `Application`.
///
/// `active` is the master flag — when true, the main window's
/// `tick_and_render` short-circuits to `tick_perform_mode` and the input
/// handlers route through the perform-mode helpers.
///
/// `pending_enter` / `pending_exit` are deferred-action flags drained
/// from the `about_to_wait` block (so window mutations happen with the
/// `ActiveEventLoop` in scope, matching how `pending_toggle_output`
/// works).
pub(crate) struct PerformModeState {
    /// Master perform-mode flag. When true, the main window UI is
    /// replaced with the perform-mode HUD and all normal UI ticks are
    /// skipped. The content thread and output window are completely
    /// untouched.
    pub(crate) active: bool,

    /// Set by the Perform header button — handled in `about_to_wait`.
    pub(crate) pending_enter: bool,

    /// Set by the exit button, Escape key, or output-window-closed
    /// detection — handled in `about_to_wait`.
    pub(crate) pending_exit: bool,

    /// Cached hit-test rect (logical pixels) of the exit button drawn in
    /// performance mode. Updated each perform-mode frame from window size.
    pub(crate) exit_button_rect: Rect,

    /// Hover state of the exit button — drives color change.
    pub(crate) exit_button_hover: bool,
}

impl PerformModeState {
    pub(crate) fn new() -> Self {
        Self {
            active: false,
            pending_enter: false,
            pending_exit: false,
            exit_button_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            exit_button_hover: false,
        }
    }
}

impl Default for PerformModeState {
    fn default() -> Self {
        Self::new()
    }
}
