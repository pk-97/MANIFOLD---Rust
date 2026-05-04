//! Perform-mode entry / exit. Called from `Application::about_to_wait`.

use winit::event_loop::ActiveEventLoop;

use crate::app::Application;

impl Application {
    /// Drain `pending_enter` / `pending_exit` flags and apply them. Called
    /// from `about_to_wait` so window creation/teardown happens with the
    /// `ActiveEventLoop` in scope (matches `pending_toggle_output`).
    pub(crate) fn handle_perform_mode_pending(&mut self, event_loop: &ActiveEventLoop) {
        // Enter performance mode: open output window (same call as Monitor)
        // and switch the main window to a single Exit button. If the output
        // window fails to open, abort entry — never enter perform mode without
        // an audience output.
        if self.perform.pending_enter {
            self.perform.pending_enter = false;
            if !self.perform.active {
                if !self.window_registry.has_output_window() {
                    self.open_output_window(event_loop, "Output", None, false);
                }
                if self.window_registry.has_output_window() {
                    // Quiesce in-flight UI state so nothing is left dangling
                    // when we resume normal mode on exit.
                    self.text_input.cancel();
                    self.ws.ui_root.dropdown.close(&mut self.ws.ui_root.tree);
                    self.ws.ui_root.browser_popup.close();
                    self.ws.ui_root.ableton_picker.close();
                    if self.mouse_pressed {
                        self.ws.ui_root.pointer_event(
                            self.cursor_pos,
                            manifold_ui::input::PointerAction::Up,
                            self.time_since_start,
                        );
                        self.mouse_pressed = false;
                    }
                    self.perform.exit_button_hover = false;
                    self.perform.active = true;
                    self.ws.offscreen_dirty = true;
                    log::info!("[Perform] Entered performance mode");
                } else {
                    log::warn!(
                        "[Perform] Output window failed to open — aborting performance mode entry"
                    );
                }
            }
        }

        // Exit performance mode: restore normal UI. Output window is left
        // open intentionally — if the user exited by accident mid-show the
        // audience never goes black.
        if self.perform.pending_exit {
            self.perform.pending_exit = false;
            if self.perform.active {
                self.perform.active = false;
                self.perform.exit_button_hover = false;
                // Force a full UI rebuild on the next frame so panels resync
                // from the latest content snapshot.
                self.needs_rebuild = true;
                self.needs_structural_sync = true;
                self.ws.ui_root.tree.mark_all_dirty();
                if let Some(cm) = &mut self.ui_cache_manager {
                    cm.invalidate_all();
                }
                self.ws.offscreen_dirty = true;
                log::info!("[Perform] Exited performance mode");
            }
        }
    }
}
