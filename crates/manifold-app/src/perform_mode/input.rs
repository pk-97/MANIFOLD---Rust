//! Perform-mode input gating. Called from `app.rs` event handlers when
//! `perform.active` is true. Each handler returns `true` if perform mode
//! consumed the event, in which case the caller should `return` early
//! and skip all normal UI input processing.

use manifold_ui::node::Vec2;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::Key;

use crate::app::Application;

impl Application {
    /// Hover hit-test for the exit button. Returns true if perform mode
    /// consumed the event.
    pub(crate) fn perform_handle_cursor_moved(&mut self, cursor_pos: Vec2) -> bool {
        if !self.perform.active {
            return false;
        }
        let was_hover = self.perform.exit_button_hover;
        let is_hover = self.perform.exit_button_rect.contains(cursor_pos);
        if was_hover != is_hover {
            self.perform.exit_button_hover = is_hover;
            self.offscreen_dirty = true;
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
        if button == MouseButton::Left
            && state == ElementState::Released
            && self.perform.exit_button_rect.contains(self.cursor_pos)
        {
            self.perform.pending_exit = true;
        }
        true
    }

    /// Keyboard handler. All keys are dropped — hardware controllers /
    /// OSC drive playback during a live show. Exit only via the on-screen
    /// button so an accidental key press can't kill the HUD mid-show.
    pub(crate) fn perform_handle_key(&mut self, logical_key: &Key) -> bool {
        if !self.perform.active {
            return false;
        }
        let _ = logical_key;
        true
    }

    /// Cursor-left handler. Clears the exit button hover state.
    pub(crate) fn perform_handle_cursor_left(&mut self) -> bool {
        if !self.perform.active {
            return false;
        }
        if self.perform.exit_button_hover {
            self.perform.exit_button_hover = false;
            self.offscreen_dirty = true;
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
