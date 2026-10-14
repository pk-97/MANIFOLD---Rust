//! Perform-mode input gating. Called from `app.rs` event handlers when
//! `perform.active` is true. Each handler returns `true` if perform mode
//! consumed the event, in which case the caller should `return` early
//! and skip all normal UI input processing. The handling itself is the shared
//! seam in `frame.rs`.

use manifold_ui::node::Vec2;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::Key;

use super::frame;
use crate::app::Application;

impl Application {
    /// Hover hit-test for the exit button. Returns true if perform mode
    /// consumed the event.
    pub(crate) fn perform_handle_cursor_moved(&mut self, cursor_pos: Vec2) -> bool {
        if !self.perform.active {
            return false;
        }
        if frame::pointer_moved(&mut self.perform, cursor_pos) {
            self.ws.offscreen_dirty = true;
        }
        true
    }

    /// Mouse-button hit-test for the exit button. Only the exit button
    /// responds to clicks; all other input is dropped.
    pub(crate) fn perform_handle_mouse_input(
        &mut self,
        button: MouseButton,
        state: ElementState,
    ) -> bool {
        if !self.perform.active {
            return false;
        }
        if button == MouseButton::Left && state == ElementState::Released {
            frame::pointer_released(&mut self.perform, self.cursor_pos);
        }
        true
    }

    /// Keyboard handler: every key is dropped (`frame::takes_keys`).
    pub(crate) fn perform_handle_key(&mut self, _logical_key: &Key) -> bool {
        frame::takes_keys(&self.perform)
    }

    /// Cursor-left handler. Clears the exit button hover state.
    pub(crate) fn perform_handle_cursor_left(&mut self) -> bool {
        if !self.perform.active {
            return false;
        }
        if frame::pointer_left(&mut self.perform) {
            self.ws.offscreen_dirty = true;
        }
        true
    }

    /// Mouse-wheel handler. Always consumes the event in perform mode.
    pub(crate) fn perform_handle_mouse_wheel(&mut self) -> bool {
        self.perform.active
    }

    /// Hook called from output-window close paths. If perform mode is
    /// active when the audience output disappears, schedule an exit.
    pub(crate) fn perform_on_output_window_closed(&mut self) {
        if self.perform.active {
            self.perform.pending_exit = true;
        }
    }
}
