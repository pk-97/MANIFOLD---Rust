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
                    // `--resume` (GIG_RESILIENCE_DESIGN section 5.2 step 2) targets
                    // the display captured in the breadcrumb; every other
                    // caller of perform-mode entry leaves this `None` and
                    // keeps the existing "first non-primary" default inside
                    // `open_output_window` unchanged.
                    let display_index = self.pending_resume.take().and_then(|pending| {
                        crate::breadcrumb::resolve_display_index(
                            event_loop,
                            pending.display_uuid.as_deref(),
                        )
                    });
                    self.open_output_window(event_loop, "Output", display_index, false);
                }
                if self.window_registry.has_output_window() {
                    let held_pointer = self.mouse_pressed.then_some((self.cursor_pos, self.time_since_start));
                    super::frame::enter(&mut self.perform, &mut self.ws.ui_root, &mut self.text_input, held_pointer);
                    self.mouse_pressed = false;
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
                // Rebuild the frozen tree. The planner compares against the
                // last normal frame's inputs, so whatever moved during the
                // set re-projects on the first frame back.
                self.needs_rebuild = true;
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
