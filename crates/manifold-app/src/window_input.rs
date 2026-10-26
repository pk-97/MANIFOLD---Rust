//! The window input owner.
//!
//! Both the timeline/inspector window and the graph-editor window enter their
//! pointer / scroll / keyboard handling through the `input_*` dispatchers here.
//! `App::window_event` is a thin router: each input arm is one delegation into
//! this module, and the `is_graph_editor` / `is_primary` branching lives in the
//! dispatcher — not smeared across the match. Now there is one owner.
//!
//! The shared core both windows route through:
//! - **Gesture production** — `UIInputSystem` (`ui_root.input`), already the
//!   same type on both `Workspace`s.
//! - **Scroll normalization** — [`normalize_scroll_delta`] (one line-delta→pixel
//!   rule for the primary scroll, the dropdown scroll, and the editor zoom).
//! - **Cursor projection** — [`Application::logical_cursor`] (physical→logical).
//!
//! The keyboard text-input `match` blocks stay window-specific *by design*: the
//! search field, the WGSL code field, and the mapping-popover field have
//! genuinely different Enter / Escape / `typing`-gating policy. They delegate to
//! the same `TextInput` methods (the real shared core); merging the policy
//! matches would be a behaviour change, not a dedup.
//!
//! Editor input never asks for a repaint: what it changes reaches the editor's
//! composite through an input the composite reads (`editor_frame::needs_composite`).

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::{Key, NamedKey};
use winit::window::WindowId;

use manifold_ui::cursors::TimelineCursor;
use manifold_ui::input::PointerAction;
use manifold_ui::interaction_overlay::DragMode;
use manifold_ui::node::Vec2;

use crate::app::Application;
use crate::content_command::ContentCommand;
use crate::window_registry::WindowRole;

/// One mouse-wheel notch in logical pixels. A `LineDelta` notch is this many
/// pixels; a `PixelDelta` (trackpad) is already in pixels. The single rule for
/// every scroll consumer — primary timeline scroll, the open-dropdown scroll,
/// and the editor's cursor-anchored zoom — so a notch means the same everywhere.
pub(crate) const LINE_DELTA_PX: f32 = 20.0;

/// Normalize a winit scroll delta to logical pixels `(dx, dy)`.
///
/// `LineDelta` (mouse wheel) is scaled by [`LINE_DELTA_PX`] per notch;
/// `PixelDelta` (trackpad) passes through. Downstream consumers apply their own
/// speed constant on top.
pub(crate) fn normalize_scroll_delta(delta: MouseScrollDelta) -> (f32, f32) {
    match delta {
        MouseScrollDelta::LineDelta(x, y) => (x * LINE_DELTA_PX, y * LINE_DELTA_PX),
        MouseScrollDelta::PixelDelta(pos) => (pos.x as f32, pos.y as f32),
    }
}

/// A window's size in logical pixels; `(1, 1)` if it is gone.
fn editor_logical_size(
    registry: &crate::window_registry::WindowRegistry,
    window_id: WindowId,
) -> (f32, f32) {
    registry.get(&window_id).map_or((1.0, 1.0), |ws| {
        let s = ws.window.scale_factor() as f32;
        let sz = ws.window.inner_size();
        (sz.width as f32 / s, sz.height as f32 / s)
    })
}

pub(crate) fn native_timeline_cursor(cursor: TimelineCursor) -> winit::window::CursorIcon {
    match cursor {
        TimelineCursor::Default => winit::window::CursorIcon::Default,
        TimelineCursor::Crosshair => winit::window::CursorIcon::Crosshair,
        TimelineCursor::ResizeHorizontal => winit::window::CursorIcon::ColResize,
        TimelineCursor::ResizeVertical => winit::window::CursorIcon::RowResize,
        TimelineCursor::Move => winit::window::CursorIcon::Move,
        TimelineCursor::Blocked => winit::window::CursorIcon::NotAllowed,
        TimelineCursor::Bend => winit::window::CursorIcon::Grab,
        TimelineCursor::Grabbing => winit::window::CursorIcon::Grabbing,
        TimelineCursor::Pointer => winit::window::CursorIcon::Pointer,
    }
}

impl Application {
    /// Shared modifier state for native window events and live UI gestures.
    pub(crate) fn input_modifiers(&mut self, modifiers: manifold_ui::input::Modifiers) {
        self.modifiers = modifiers;
        self.overlay.set_modifiers(modifiers);
        self.ws.ui_root.input.set_modifiers(modifiers);
    }
    /// Physical→logical cursor position using `window_id`'s scale factor. The
    /// one place that conversion lives; both the primary cursor track and the
    /// editor's zoom anchor read it.
    pub(crate) fn logical_cursor(
        &self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
    ) -> Vec2 {
        let scale = self
            .window_registry
            .get(&window_id)
            .map(|ws| ws.window.scale_factor())
            .unwrap_or(1.0);
        Vec2::new(
            position.x as f32 / scale as f32,
            position.y as f32 / scale as f32,
        )
    }

    /// Whether `last_click` (a handle's own timestamp, e.g.
    /// `split_handle_last_click`) is recent enough to make THIS press a
    /// double-click. Same threshold and shape as the output window's
    /// `output_last_click` check — the one double-click primitive every raw
    /// (pre-`UIEvent`-pipeline) click target in this file uses.
    pub(crate) fn is_double_click(&self, last_click: Option<std::time::Instant>) -> bool {
        const DOUBLE_CLICK_MS: u128 = 300;
        last_click
            .map(|t| std::time::Instant::now().duration_since(t).as_millis() < DOUBLE_CLICK_MS)
            .unwrap_or(false)
    }

    /// Push the cursor-manager's pending shape to `window_id` if it changed.
    /// The one place the `TimelineCursor → winit::CursorIcon` mapping lives;
    /// both the primary timeline cursor track and the editor's divider-hover
    /// cursor go through it.
    pub(crate) fn apply_pending_cursor(&mut self, window_id: WindowId) {
        if self.cursor_manager.needs_update()
            && let Some(ws) = self.window_registry.get(&window_id)
        {
            let icon = native_timeline_cursor(self.cursor_manager.pending_cursor_icon());
            ws.window.set_cursor(icon);
            self.cursor_manager.mark_applied();
        }
    }

    // ── Text-edit session pointer routing (UI_WIDGET_UNIFICATION P5b/D16) ──
    // One `TextInputState` serves both windows, so one gate covers both. The
    // main tree measures for both (every tree carries an identical
    // `CachedTextMeasure`); nothing measures before a renderer exists. The
    // mechanics are the shared seam in `text_session.rs`.

    /// Arrow keys in a text session, both windows.
    pub(crate) fn text_input_arrow(&mut self, key: winit::keyboard::NamedKey) {
        use crate::text_session::Arrow;
        use winit::keyboard::NamedKey;
        let arrow = match key {
            NamedKey::ArrowLeft => Arrow::Left,
            NamedKey::ArrowRight => Arrow::Right,
            NamedKey::ArrowUp => Arrow::Up,
            NamedKey::ArrowDown => Arrow::Down,
            _ => return,
        };
        let tree = self.ui_renderer.as_ref().map(|_| &self.ws.ui_root.tree);
        crate::text_session::arrow(&mut self.text_input, tree, arrow, self.modifiers);
    }

    /// A left press while a session may be up. True when the field consumed
    /// it; a press outside commits first and returns false, so it still
    /// reaches its normal target.
    pub(crate) fn text_input_pointer_down(&mut self, pos: Vec2) -> bool {
        use crate::text_session::PointerDown;
        let tree = self.ui_renderer.as_ref().map(|_| &self.ws.ui_root.tree);
        match crate::text_session::pointer_down(
            &mut self.text_input,
            tree,
            pos,
            self.time_since_start,
            self.modifiers.shift,
        ) {
            PointerDown::Idle => false,
            PointerDown::Inside => true,
            PointerDown::Committed(field, text) => {
                self.handle_text_input_commit(field, &text);
                false
            }
        }
    }

    /// Pointer motion extends an armed text drag; false when none is armed.
    pub(crate) fn text_input_pointer_move(&mut self, pos: Vec2) -> bool {
        let tree = self.ui_renderer.as_ref().map(|_| &self.ws.ui_root.tree);
        crate::text_session::pointer_move(&mut self.text_input, tree, pos)
    }

    /// A release ends a text drag.
    pub(crate) fn text_input_pointer_up(&mut self) {
        crate::text_session::pointer_up(&mut self.text_input);
    }

    // ── Dispatchers: the single owner entry per winit input event ──────────
    // `window_event` calls exactly one of these per arm; the primary / editor /
    // output-window split lives here, not in the match.

    /// `CursorMoved` for any window.
    pub(crate) fn input_cursor_moved(
        &mut self,
        window_id: WindowId,
        is_primary: bool,
        is_graph_editor: bool,
        position: PhysicalPosition<f64>,
    ) {
        if self.ws.ui_root.export_progress.is_open() {
            if is_primary {
                self.primary_cursor_moved(window_id, position);
            }
            return;
        }
        // An active text session's drag claims pointer motion ahead of
        // everything else (P5b) — but only actually consumes it while a
        // drag is armed (`text_input_pointer_move` returns `false`
        // otherwise), so ordinary hover/drag handling is untouched when no
        // session is open.
        if self.text_input_pointer_move(self.logical_cursor(window_id, position)) {
            return;
        }
        if is_graph_editor {
            self.editor_cursor_moved(window_id, position);
        } else if is_primary {
            self.primary_cursor_moved(window_id, position);
        }
    }

    /// `MouseInput` for any window.
    pub(crate) fn input_mouse_input(
        &mut self,
        window_id: WindowId,
        is_primary: bool,
        is_graph_editor: bool,
        button: MouseButton,
        state: ElementState,
    ) {
        if self.ws.ui_root.export_progress.is_open() {
            if is_primary {
                self.primary_mouse_input(window_id, is_primary, button, state);
            }
            return;
        }
        // An active text session claims the press/release ahead of normal
        // dispatch (P5b/D16): inside the field it places the caret/word and
        // consumes the event; outside it commits first and then falls
        // through so the click still reaches its normal target. `MouseInput`
        // carries no position (winit), so read it from wherever each window
        // already tracks it: `self.cursor_pos` for the main window, the
        // canvas's own tracked cursor for the editor window (same source
        // `editor_mouse_input`'s picker branch uses, above).
        if button == MouseButton::Left {
            let press_pos = if is_graph_editor {
                self.graph_canvas
                    .as_ref()
                    .map(|c| Vec2::new(c.cursor().0, c.cursor().1))
                    .unwrap_or(Vec2::ZERO)
            } else {
                self.cursor_pos
            };
            match state {
                ElementState::Pressed => {
                    if self.text_input_pointer_down(press_pos) {
                        return;
                    }
                }
                ElementState::Released => {
                    self.text_input_pointer_up();
                }
            }
        }
        if is_graph_editor {
            self.editor_mouse_input(window_id, button, state);
        } else {
            self.primary_mouse_input(window_id, is_primary, button, state);
        }
    }

    /// `MouseWheel` for any window.
    pub(crate) fn input_mouse_wheel(
        &mut self,
        window_id: WindowId,
        is_primary: bool,
        is_graph_editor: bool,
        delta: MouseScrollDelta,
    ) {
        if self.ws.ui_root.export_progress.is_open() {
            return;
        }
        if is_graph_editor {
            // The editor's zoom is self-contained; the primary scroll block
            // below is `is_primary`-gated and would never run for the editor
            // window, so the old bool return is moot here.
            self.editor_mouse_wheel(window_id, delta);
        } else {
            self.primary_mouse_wheel(is_primary, delta);
        }
    }

    /// `CursorMoved` in the timeline/inspector window: cursor tracking, perform-
    /// mode handoff, split / inspector-resize drags, then the shared
    /// `UIInputSystem` move + the timeline `InteractionOverlay` hover.
    pub(crate) fn primary_cursor_moved(
        &mut self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
    ) {
        // Convert to logical pixels
        self.cursor_pos = self.logical_cursor(window_id, position);

        if self.perform_handle_cursor_moved(self.cursor_pos) {
            return;
        }

        // A gizmo drag on the preview holds the pointer until release.
        if self.ws.preview_gizmo.dragging() {
            self.preview_gizmo_drag_to();
            return;
        }
        if self.preview_navigation_move() {
            return;
        }

        // Split handle drag takes highest priority
        // From Unity PanelResizeHandle.OnDrag
        if self.split_dragging {
            self.ws
                .ui_root
                .layout
                .update_split_from_drag(self.cursor_pos.y);
            self.cursor_manager.set(TimelineCursor::ResizeVertical);
        }
        // Inspector resize drag takes next priority
        else if self.ws.ui_root.inspector_resize_dragging {
            self.ws.ui_root.update_inspector_resize(self.cursor_pos.x);
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
        }
        // Audio Setup dock resize drag (D1)
        else if self.ws.ui_root.audio_setup_resize_dragging {
            self.ws.ui_root.update_audio_setup_resize(self.cursor_pos.x);
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
        } else {
            self.ws.ui_root.pointer_event(
                self.cursor_pos,
                PointerAction::Move,
                self.time_since_start,
            );

            if self.ws.ui_root.background_input_blocked() {
                // A modal popup is open: keep tree hover feedback on the popup,
                // but don't run the timeline InteractionOverlay or background
                // cursor-shape feedback (split-handle, resize, etc.).
                self.cursor_manager.set_default();
                if self.selection.automation_feedback.take().is_some() {
                    self.ws.offscreen_dirty = true;
                }
            } else {
                // Route hover through InteractionOverlay (port of Unity OnPointerMove).
                // This handles: CursorBeat/CursorLayerIndex tracking, per-layer bitmap
                // invalidation on hover change, and cursor shape feedback.
                if let Some(content_tx) = self.content_tx.as_ref() {
                    let mut host = crate::editing_host::AppEditingHost::new(
                        self.local_project.edit(&mut self.ui_edits, content_tx),
                        content_tx,
                        &self.content_state,
                        &mut self.cursor_manager,
                        &mut self.active_layer_id,
                        &mut self.ws.offscreen_dirty,
                    );
                    self.overlay.on_pointer_move(
                        self.cursor_pos,
                        &mut host,
                        &mut self.selection,
                        &self.ws.ui_root.viewport,
                    );
                }

                // Update cursor based on current interaction state.
                // From Unity: Cursors.SetMove/SetBlocked/SetResizeHorizontal/SetDefault
                self.update_cursor_for_position();
            }
        }

        self.apply_pending_cursor(window_id);
    }

    /// Menus retain their source focus; direct presses choose the card host.
    fn update_card_keyboard_focus(&mut self) {
        let on_picture = self
            .preview_fit()
            .is_some_and(|fit| fit.contains(self.cursor_pos));
        let ui = &mut self.ws.ui_root;
        if ui.background_input_blocked() {
            return;
        }
        let in_inspector = ui.layout.inspector().contains(self.cursor_pos);
        // A click on the Scene panel's picture picks its items, so the item
        // keys (copy, paste, duplicate, delete) follow it there too.
        let on_scene_picture = on_picture
            && ui.scene_setup_panel.live_layer_id().is_some()
            && !ui.overlay_contains_point(self.cursor_pos);
        let in_objects = (ui.scene_setup_panel.is_open()
            && ui.inspector.scene_content_rect().contains(self.cursor_pos))
            || on_scene_picture;
        if !in_inspector && self.input_handler.inspector_has_focus {
            ui.inspector.clear_effect_selection(&mut ui.tree);
        }
        ui.object_cards_have_focus = in_objects;
        self.input_handler.inspector_has_focus = in_inspector || in_objects;
    }

    /// `MouseInput` in the timeline/inspector window (`is_primary`) or the output
    /// window (the `else` — double-click toggles borderless presentation).
    pub(crate) fn primary_mouse_input(
        &mut self,
        window_id: WindowId,
        is_primary: bool,
        button: MouseButton,
        state: ElementState,
    ) {
        if is_primary && self.perform_handle_mouse_input(button, state) {
            if manifold_ui::input::input_trace_enabled() {
                eprintln!("[input-trace] window: {button:?} {state:?} consumed by perform mode");
            }
            return;
        }
        if is_primary {
            match button {
                MouseButton::Left => {
                    match state {
                        ElementState::Pressed => {
                            self.mouse_pressed = true;

                            self.update_card_keyboard_focus();

                            if self.ws.ui_root.object_cards_have_focus
                                && !self.ws.ui_root.background_input_blocked()
                                && let Some(node) = self.ws.ui_root.tree.hit_test(self.cursor_pos)
                            {
                                let ui = &mut self.ws.ui_root;
                                ui.scene_setup_panel.select_outliner_node(node);
                            }

                            if self.ws.ui_root.background_input_blocked() {
                                // A modal is open: its full-screen scrim owns
                                // every press, including one that lands on a
                                // seam rect behind it. The per-branch D5 guard
                                // (`overlay_contains_point`) only covers the
                                // popup's container rect, not the scrim, so it
                                // can't see this case — without this gate the
                                // press starts a background resize drag through
                                // the open popup. Route through normal dispatch
                                // (the scrim captures / dismisses) instead.
                                self.ws.ui_root.pointer_event(
                                    self.cursor_pos,
                                    PointerAction::Down,
                                    self.time_since_start,
                                );
                            } else if self.ws.ui_root.layout.is_near_split_handle(self.cursor_pos)
                                && !self.ws.ui_root.overlay_contains_point(self.cursor_pos)
                            {
                                // The seam
                                // is visually UNDER a floating overlay when
                                // one occupies this point (e.g. the Audio
                                // Setup panel docked over the timeline) — in
                                // that case this branch yields and the press
                                // falls through to the final `else` below,
                                // which routes it through normal overlay/
                                // panel dispatch instead of stealing it for a
                                // split-handle drag (BUG-059's window-seam
                                // class).
                                //
                                // P2 "panel-split snap-back" (D15): a double-
                                // click resets the split to its default
                                // instead of starting a drag — checked before
                                // the drag-start below, same
                                // timestamp-comparison shape as
                                // `output_last_click`'s double-click (this
                                // press never reaches the generic
                                // `Gesture::DoubleClick` pipeline, it's
                                // intercepted here exactly like the single-
                                // click drag-start already was).
                                if self.is_double_click(self.split_handle_last_click) {
                                    self.split_handle_last_click = None;
                                    self.ws.ui_root.layout.reset_timeline_split();
                                } else {
                                    self.split_handle_last_click = Some(std::time::Instant::now());
                                    // Begin video/timeline split drag.
                                    // From Unity PanelResizeHandle.OnPointerDown.
                                    self.split_dragging = true;
                                    self.ws.ui_root.set_split_handle_drag();
                                    if manifold_ui::input::input_trace_enabled() {
                                        eprintln!(
                                            "[input-trace] window: PRESS ({:.0},{:.0}) intercepted \
                                             → split-handle drag",
                                            self.cursor_pos.x, self.cursor_pos.y
                                        );
                                    }
                                }
                            } else if self.ws.ui_root.is_near_audio_setup_edge(self.cursor_pos)
                                && !self.ws.ui_root.overlay_contains_point(self.cursor_pos)
                            {
                                // Audio Setup dock resize handle (D1) — its LEFT
                                // edge. Double-click snaps the width back to the
                                // default; a single press begins the drag.
                                if self.is_double_click(self.audio_setup_handle_last_click) {
                                    self.audio_setup_handle_last_click = None;
                                    self.ws.ui_root.layout.reset_audio_setup_width();
                                } else {
                                    self.audio_setup_handle_last_click =
                                        Some(std::time::Instant::now());
                                    self.ws.ui_root.begin_audio_setup_resize(self.cursor_pos.x);
                                    self.ws.ui_root.set_audio_setup_handle_drag();
                                }
                            } else if self.ws.ui_root.is_near_inspector_edge(self.cursor_pos)
                                && !self.ws.ui_root.overlay_contains_point(self.cursor_pos)
                            {
                                // D5 — same seam-yields-to-overlay guard as
                                // the split handle above.
                                if self.is_double_click(self.inspector_handle_last_click) {
                                    self.inspector_handle_last_click = None;
                                    self.ws.ui_root.layout.reset_inspector_width();
                                } else {
                                    self.inspector_handle_last_click =
                                        Some(std::time::Instant::now());
                                    self.ws.ui_root.begin_inspector_resize(self.cursor_pos.x);
                                    self.ws.ui_root.set_inspector_handle_drag();
                                    if manifold_ui::input::input_trace_enabled() {
                                        eprintln!(
                                            "[input-trace] window: PRESS ({:.0},{:.0}) intercepted \
                                             → inspector-resize drag",
                                            self.cursor_pos.x, self.cursor_pos.y
                                        );
                                    }
                                }
                            } else if !self.preview_gizmo_press()
                                && !self.preview_navigation_press(MouseButton::Left)
                            {
                                self.ws.ui_root.pointer_event(
                                    self.cursor_pos,
                                    PointerAction::Down,
                                    self.time_since_start,
                                );
                            }
                        }
                        ElementState::Released => {
                            self.mouse_pressed = false;
                            if manifold_ui::input::input_trace_enabled() {
                                let route = if self.split_dragging {
                                    "ends split drag"
                                } else if self.ws.ui_root.inspector_resize_dragging {
                                    "ends inspector resize"
                                } else {
                                    "forwarded to UI"
                                };
                                eprintln!(
                                    "[input-trace] window: RELEASE ({:.0},{:.0}) {route}",
                                    self.cursor_pos.x, self.cursor_pos.y
                                );
                            }
                            self.preview_gizmo_release_click();
                            if self.preview_gizmo_release() || self.preview_navigation_release() {
                                // A preview gizmo or navigation drag held the pointer.
                            } else if self.split_dragging {
                                // End video/timeline split drag.
                                // From Unity PanelResizeHandle.OnPointerUp.
                                self.split_dragging = false;
                                self.cursor_manager.set_default();
                                self.ws.ui_root.set_split_handle_idle();
                                // Width/ratio are captured at save time from
                                // the live UI layout (stamp_save_state).
                            } else if self.ws.ui_root.inspector_resize_dragging {
                                self.ws.ui_root.end_inspector_resize();
                            } else if self.ws.ui_root.audio_setup_resize_dragging {
                                self.ws.ui_root.end_audio_setup_resize();
                                self.cursor_manager.set_default();
                                self.ws.ui_root.set_audio_setup_handle_idle();
                            } else {
                                self.ws.ui_root.pointer_event(
                                    self.cursor_pos,
                                    PointerAction::Up,
                                    self.time_since_start,
                                );
                            }
                        }
                    }
                }
                MouseButton::Middle => {
                    if state == ElementState::Pressed {
                        self.preview_navigation_press(MouseButton::Middle);
                    } else {
                        self.preview_navigation_release();
                    }
                }
                MouseButton::Right => {
                    if state == ElementState::Pressed {
                        self.update_card_keyboard_focus();
                        self.ws.ui_root.right_click(self.cursor_pos);
                    }
                }
                _ => {}
            }
        } else if button == MouseButton::Left && state == ElementState::Pressed {
            // Double-click on the output window toggles a dedicated
            // borderless presentation window instead of mutating the
            // existing titled window in place.
            const DOUBLE_CLICK_MS: u128 = 300;
            let now = std::time::Instant::now();
            let is_double = self
                .output_last_click
                .map(|t| now.duration_since(t).as_millis() < DOUBLE_CLICK_MS)
                .unwrap_or(false);

            if is_double {
                self.output_last_click = None;
                // Toggle fullscreen by resizing the existing window in place.
                // Do NOT destroy/recreate — that disrupts the CVDisplayLink
                // cadence and tears the output.
                if let Some(ws) = self.window_registry.get_mut(&window_id)
                    && let WindowRole::Output {
                        ref mut presentation,
                        ..
                    } = ws.role
                {
                    let new_presentation = !*presentation;
                    *presentation = new_presentation;

                    if new_presentation {
                        // → Fullscreen: save frame, expand to monitor
                        let outer = ws.window.outer_position().unwrap_or_default();
                        let inner = ws.window.inner_size();
                        self.output_saved_frame = Some([
                            outer.x as f64,
                            outer.y as f64,
                            inner.width as f64,
                            inner.height as f64,
                        ]);
                        if let Some(monitor) = ws.window.current_monitor() {
                            let mon_pos = monitor.position();
                            let mon_size = monitor.size();
                            let scale = monitor.scale_factor();
                            let lw = mon_size.width as f64 / scale;
                            let lh = mon_size.height as f64 / scale;
                            let lx = mon_pos.x as f64 / scale;
                            let ly = mon_pos.y as f64 / scale;
                            ws.window.set_decorations(false);
                            let _ = ws
                                .window
                                .request_inner_size(winit::dpi::LogicalSize::new(lw, lh));
                            ws.window
                                .set_outer_position(winit::dpi::LogicalPosition::new(lx, ly));
                        }
                        // Set window level above menu bar (NSMainMenuWindowLevel=24)
                        // so our borderless window covers it on this
                        // display only — no global setPresentationOptions.
                        #[cfg(target_os = "macos")]
                        crate::edr_surface::set_window_level(&ws.window, 25);
                        // Tahoe draws a 1px border around shadowed windows
                        // even when borderless — drop it for presentation.
                        #[cfg(target_os = "macos")]
                        crate::edr_surface::set_window_shadow(&ws.window, false);
                    } else {
                        // → Windowed: restore saved frame + menu bar
                        ws.window.set_decorations(true);
                        #[cfg(target_os = "macos")]
                        crate::edr_surface::set_window_shadow(&ws.window, true);
                        if let Some(frame) = self.output_saved_frame.take() {
                            ws.window
                                .set_outer_position(winit::dpi::PhysicalPosition::new(
                                    frame[0], frame[1],
                                ));
                            let _ = ws.window.request_inner_size(winit::dpi::PhysicalSize::new(
                                frame[2], frame[3],
                            ));
                        }
                        // Restore NSNormalWindowLevel=0 so the menu
                        // bar is no longer covered.
                        #[cfg(target_os = "macos")]
                        crate::edr_surface::set_window_level(&ws.window, 0);
                    }

                    let new_size = ws.window.inner_size();
                    self.send_content_cmd(ContentCommand::ResizeOutputSurface(
                        new_size.width.max(1),
                        new_size.height.max(1),
                    ));
                }
            } else {
                self.output_last_click = Some(now);
            }
        }
    }

    /// `MouseWheel` in the timeline/inspector window: perform-mode handoff, open-
    /// overlay scroll, then timeline zoom / pan / vertical scroll.
    pub(crate) fn primary_mouse_wheel(&mut self, is_primary: bool, delta: MouseScrollDelta) {
        if is_primary && self.perform_handle_mouse_wheel() {
            return;
        }
        if is_primary {
            // Modal pickers must receive the wheel before background scrolling
            // is blocked. The overlay driver handles their viewport hit-test
            // and schedules a redraw, just as it does for the dropdown.
            if self.ws.ui_root.dropdown.is_open() || self.ws.ui_root.background_input_blocked() {
                let (dx, dy) = normalize_scroll_delta(delta);
                self.ws
                    .ui_root
                    .input
                    .process_scroll(self.cursor_pos, Vec2::new(dx, dy));
                return;
            }
            if self.preview_navigation_wheel(delta) {
                return;
            }
            // Convert line deltas (mouse wheel notches) to logical pixels.
            // Each downstream consumer applies its own speed constant on top.
            let (dx, dy) = normalize_scroll_delta(delta);

            let pos = self.cursor_pos;
            let inspector_rect = self.ws.ui_root.layout.inspector();
            let tracks_rect = self.ws.ui_root.layout.timeline_tracks();

            // BUG-199/BUG-223: the Audio Setup dock and the Scene tab — route
            // through the generic `UIEvent::Scroll` pipeline (same mechanism
            // the open-dropdown branch above uses) so the real app and the
            // headless `Gesture::Scroll` harness share one path. `contains()`
            // is already the open-check: a closed dock's rect is
            // `Rect::ZERO`. BUG-199's original fix assumed "the docks
            // rebuild every frame" — they rebuild when their view or scroll input moves
            // (UI_INVALIDATION P5a: the scene panel bumps its view version, the
            // dock its scroll offset).
            if self.ws.ui_root.inspector.scene_content_rect().contains(pos)
                || self.ws.ui_root.layout.audio_setup().contains(pos)
            {
                self.ws
                    .ui_root
                    .input
                    .process_scroll(self.cursor_pos, Vec2::new(dx, dy));
                return;
            }

            if inspector_rect.contains(pos) {
                // Scroll the inspector in place — offset the content nodes (the
                // slot is invalidated once per frame via `take_scrolled_in_place`)
                // instead of the old full `ui_root.build()` + `invalidate_all()`
                // (whole-atlas clear + all 7 panels) that ran every scroll frame
                // and could flash the inspector blank. Falls back to the rebuild
                // only when nothing is built yet (so the offset never
                // double-applies).
                if !self.ws.ui_root.try_inspector_scroll(dy, pos.x) {
                    self.ws.ui_root.inspector.handle_scroll_at(dy, pos.x);
                }
            } else if tracks_rect.contains(pos) {
                if self.modifiers.alt {
                    // Alt + scroll Y → continuous, cursor-anchored zoom (section 24 5e).
                    if dy.abs() > 0.01 {
                        let exponent = dy / LINE_DELTA_PX;
                        self.zoom_timeline_at_cursor(
                            manifold_ui::color::ZOOM_WHEEL_STEP_PER_NOTCH.powf(exponent),
                        );
                    }
                } else if self.modifiers.shift {
                    // Shift + scroll Y → horizontal pan
                    let ppb = self.ws.ui_root.viewport.pixels_per_beat();
                    let beat_delta = dy * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                    let new_x =
                        (self.ws.ui_root.viewport.scroll_x_beats().as_f32() - beat_delta).max(0.0);
                    self.ws
                        .ui_root
                        .viewport
                        .set_scroll(new_x, self.ws.ui_root.viewport.scroll_y_px());
                    // BUG-159: a direct user horizontal gesture — playhead-follow
                    // yields to it instead of fighting it (state_sync.rs
                    // check_auto_scroll).
                    self.ws.ui_root.viewport.note_user_scroll_x();
                } else {
                    // Plain scroll → vertical track scroll
                    let new_y = (self.ws.ui_root.viewport.scroll_y_px() - dy).max(0.0);
                    self.ws
                        .ui_root
                        .viewport
                        .set_scroll(self.ws.ui_root.viewport.scroll_x_beats().as_f32(), new_y);
                }
                // Native horizontal scroll (trackpad two-finger swipe)
                if dx.abs() > 0.01 && !self.modifiers.alt {
                    let ppb = self.ws.ui_root.viewport.pixels_per_beat();
                    let beat_delta = dx * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                    let new_x =
                        (self.ws.ui_root.viewport.scroll_x_beats().as_f32() - beat_delta).max(0.0);
                    self.ws
                        .ui_root
                        .viewport
                        .set_scroll(new_x, self.ws.ui_root.viewport.scroll_y_px());
                    // BUG-159: see the Shift+scroll site above.
                    self.ws.ui_root.viewport.note_user_scroll_x();
                }
            }
        }
    }

    /// Scale pixels-per-beat by `factor` (smooth, not a jump between the fixed
    /// levels), keeping the beat under the mouse where it is on screen.
    fn zoom_timeline_at_cursor(&mut self, factor: f32) {
        let pos = self.cursor_pos;
        let beat_at_cursor = self.ws.ui_root.viewport.pixel_to_beat(pos.x).as_f32();
        let new_ppb = self.ws.ui_root.viewport.zoom_continuous(factor);
        self.ws
            .ui_root
            .viewport
            .zoom_to(new_ppb, beat_at_cursor, pos.x);
    }

    /// Trackpad pinch in any window. `magnification` is winit's per-event
    /// `PinchGesture` delta: positive = fingers spreading = zoom in. Zooms the
    /// timeline over the tracks and the graph canvas in the editor; the 3D
    /// viewport and side panels ignore it.
    pub(crate) fn input_pinch(
        &mut self,
        window_id: WindowId,
        is_primary: bool,
        is_graph_editor: bool,
        magnification: f32,
    ) {
        if !magnification.is_finite() || self.ws.ui_root.export_progress.is_open() {
            return;
        }
        if is_graph_editor {
            self.editor_pinch(window_id, magnification);
        } else if is_primary {
            if self.perform_handle_mouse_wheel()
                || self.ws.ui_root.dropdown.is_open()
                || self.ws.ui_root.background_input_blocked()
            {
                return;
            }
            if self
                .ws
                .ui_root
                .layout
                .timeline_tracks()
                .contains(self.cursor_pos)
            {
                self.zoom_timeline_at_cursor(
                    (magnification * manifold_ui::color::ZOOM_PINCH_GAIN).exp(),
                );
            }
        }
    }

    /// This App's borrow for one editor-window event through the editor-input
    /// seam (`editor_input.rs`). `None` when the editor is closed.
    fn editor_pointer(
        &mut self,
        window_id: WindowId,
    ) -> Option<crate::editor_input::EditorPointer<'_>> {
        let (w, h) = editor_logical_size(&self.window_registry, window_id);
        Some(crate::editor_input::EditorPointer {
            ws: self.graph_editor.as_mut()?,
            canvas: self.graph_canvas.as_mut(),
            mapping: &mut self.editor_mapping_popover,
            area: manifold_ui::Rect::new(0.0, 0.0, w, h),
            modifiers: self.modifiers,
            time: self.time_since_start,
            project: &self.local_project,
            content_tx: self.content_tx.as_ref(),
            is_playing: self.content_state.is_playing,
            watched: self.watched_graph_target.as_ref(),
            graph: self.content_state.active_graph_snapshot.as_deref(),
            active_layer: &mut self.active_layer_id,
            selection: &mut self.selection,
        })
    }

    /// `CursorMoved` in the editor window, through the editor-input seam. The
    /// window takes the dock's cursor; an armed gizmo drag takes the move.
    pub(crate) fn editor_cursor_moved(
        &mut self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
    ) {
        let pos = self.logical_cursor(window_id, position);
        let Some(mut cx) = self.editor_pointer(window_id) else {
            return;
        };
        let moved = crate::editor_input::cursor_moved(&mut cx, pos);
        self.cursor_manager.set(moved.cursor);
        self.apply_pending_cursor(window_id);
        if let Some(pos) = moved.gizmo_move {
            self.editor_viewport_gizmo_drag_move(pos.x, pos.y);
        }
    }

    /// Ends every editor-owned pointer gesture on focus loss or Escape
    /// (`editor_input::cancel_capture`). True when something was cancelled.
    pub(crate) fn cancel_editor_pointer_capture(&mut self, window_id: WindowId) -> bool {
        let Some(cancelled) = self
            .editor_pointer(window_id)
            .map(|mut cx| crate::editor_input::cancel_capture(&mut cx))
        else {
            return false;
        };
        self.end_gizmo_gesture(cancelled.gizmo_release);
        cancelled.any
    }

    /// P6 (`docs/REALTIME_3D_DESIGN.md` D7 Tier 2): a Left press inside the
    /// open 3D viewport, tried BEFORE the orbit-drag arm below: a handle grab
    /// on the Scene panel's selection first, then a helper that draws no
    /// pixels (`scene_gizmo::press`, shared with the main preview), then a
    /// pick of what is drawn there through the item-id buffer
    /// (`scene_pick.rs`). Reads the selection cached at the last present, so
    /// a press never rebuilds the scene. Returns `true` if the press was
    /// consumed (a drag armed, a locked handle refused, or a helper picked);
    /// the caller must not also arm an orbit drag. A pixel pick returns
    /// `false` so the orbit arm still fires: drag-on-nothing orbits.
    fn editor_viewport_gizmo_press(&mut self, cx: f32, cy: f32) -> bool {
        use crate::scene_gizmo::{GizmoPress, GizmoView};
        let Some(manifold_core::GraphTarget::Generator(layer_id)) =
            self.watched_graph_target.clone()
        else {
            return false;
        };
        if !self
            .graph_editor
            .as_ref()
            .and_then(|ed| ed.viewport_target.as_ref())
            .is_some_and(|(target, node)| {
                Some(target) == self.watched_graph_target.as_ref()
                    && Some(node) == self.last_preview_node.as_ref()
            })
        {
            return false;
        }
        let (view, mode, rect) = {
            let Some(ed) = self.graph_editor.as_ref() else {
                return false;
            };
            let Some(config) = ed
                .viewport_session
                .as_ref()
                .and_then(|session| session.displayed_config())
            else {
                return false;
            };
            let Some(rect) = ed.viewport_rect else {
                return false;
            };
            (
                GizmoView::new(config.camera.to_camera(), config.width, config.height),
                ed.viewport_gizmo_mode,
                rect,
            )
        };
        let Some((view, point)) = crate::viewport_input::gizmo_pointer(rect, view, (cx, cy)) else {
            return false;
        };
        let extend = self.modifiers.shift;
        let drawn = self
            .graph_editor
            .as_mut()
            .and_then(|ed| ed.viewport_selection.take());
        let pressed = drawn.as_ref().and_then(|drawn| {
            let at = crate::scene_gizmo::PressAt {
                view: &view,
                point,
                extend,
            };
            self.with_dispatch_ctx(|ctx| {
                crate::scene_gizmo::press(ctx, &layer_id, &drawn.scene, &drawn.selection, mode, at)
            })
        });
        let Some(ed) = self.graph_editor.as_mut() else {
            return false;
        };
        ed.viewport_selection = drawn;
        match pressed {
            Some(GizmoPress::Handle(gesture)) => {
                if gesture.is_some() {
                    ed.viewport_gizmo_drag = gesture.map(|gesture| *gesture);
                }
                true
            }
            Some(GizmoPress::Picked(true)) => true,
            // The editor viewport picks on the press, as Blender does; a drag
            // from there orbits.
            Some(GizmoPress::Picked(false)) | None => {
                let pick = crate::scene_pick::ScenePick {
                    layer: layer_id,
                    surface: crate::scene_pick::PickSurface::Viewport,
                    uv: crate::scene_pick::uv_of(&view, point),
                    extend,
                };
                self.send_content_cmd(ContentCommand::PickSceneItem(pick));
                false
            }
        }
    }

    /// Run `f` in the context a card scrub dispatches in, with the editor's
    /// watched graph as the target. `None` without a content thread.
    fn with_dispatch_ctx<R>(
        &mut self,
        f: impl FnOnce(&mut crate::ui_bridge::DispatchCtx) -> R,
    ) -> Option<R> {
        let content_tx = self.content_tx.as_ref()?;
        let mut dctx = crate::ui_bridge::DispatchCtx {
            project: self.local_project.edit(&mut self.ui_edits, content_tx),
            content_tx,
            content_state: &self.content_state,
            ui: &mut self.ws.ui_root,
            selection: &mut self.selection,
            active_layer: &mut self.active_layer_id,
            user_prefs: &mut self.user_prefs,
            editor_target: self.watched_graph_target.as_ref(),
            scrub: &mut self.scrub,
            dialogs: crate::file_dialogs::host(
                &self.window_registry,
                self.dialog_window_id,
                self.primary_window_id,
                self.current_project_path.as_deref(),
            ),
        };
        Some(f(&mut dctx))
    }

    /// Run `f` with the main preview's gizmo host and a dispatch context.
    fn with_preview_gizmo<R>(
        &mut self,
        f: impl FnOnce(
            &mut crate::physics_overlay::PreviewGizmo,
            &mut crate::ui_bridge::DispatchCtx,
        ) -> R,
    ) -> Option<R> {
        let mut host = std::mem::take(&mut self.ws.preview_gizmo);
        let out = self.with_dispatch_ctx(|ctx| f(&mut host, ctx));
        self.ws.preview_gizmo = host;
        out
    }

    /// Where the show picture sits in the main window: the same fit of the
    /// same render size the draw path blits (`ui_frame::composite_main_ui_frame`).
    pub(crate) fn preview_fit(&self) -> Option<manifold_ui::Rect> {
        crate::ui_frame::video_fit_rect(
            self.ws.ui_root.layout.video_area(),
            self.video_source_dims(),
        )
    }

    /// D7a: a left press on the main output preview, through the gizmo
    /// core's pick and the knob scrub path the editor viewport uses. True
    /// when the preview took it; a miss falls through to the UI below.
    fn preview_gizmo_press(&mut self) -> bool {
        let (fit, pos, extend) = (self.preview_fit(), self.cursor_pos, self.modifiers.shift);
        self.with_preview_gizmo(|host, ctx| host.press(ctx, fit, pos, extend))
            .unwrap_or(false)
    }

    /// A left release: a click on the preview picks what is drawn there.
    fn preview_gizmo_release_click(&mut self) {
        if self.ws.preview_gizmo.missed() {
            let (fit, pos) = (self.preview_fit(), self.cursor_pos);
            self.with_preview_gizmo(|host, ctx| host.release_click(ctx, fit, pos));
        }
    }

    /// Pointer motion while a preview drag holds the pointer.
    fn preview_gizmo_drag_to(&mut self) {
        let (fit, pos) = (self.preview_fit(), self.cursor_pos);
        self.with_preview_gizmo(|host, ctx| host.drag_to(ctx, fit, pos));
    }

    /// End a preview drag (release, Escape, focus loss, perform mode, or
    /// Show gizmos switched off): one undo entry. True when one ended.
    pub(crate) fn preview_gizmo_release(&mut self) -> bool {
        self.ws.preview_gizmo.dragging()
            && self
                .with_preview_gizmo(|host, ctx| host.release(ctx))
                .unwrap_or(false)
    }

    /// The content thread reports the rendered view of whichever scene the
    /// Scene panel shows (D7a), with Show gizmos on or off, so a click on its
    /// picture picks. Switching Show gizmos off ends a preview drag and
    /// preview navigation.
    pub(crate) fn sync_gizmo_layer(&mut self) {
        if self.ws.ui_root.scene_setup_panel.gizmo_layer().is_none() {
            self.preview_gizmo_release();
        }
        let gizmo_layer = self.ws.ui_root.scene_setup_panel.live_layer_id();
        if gizmo_layer != self.ws.gizmo_layer_sent.as_ref()
            && let Some(tx) = self.content_tx.as_ref()
        {
            self.ws.gizmo_layer_sent = gizmo_layer.cloned();
            ContentCommand::send(
                tx,
                ContentCommand::SetPhysicsOverlay(self.ws.gizmo_layer_sent.clone()),
            );
        }
        self.sync_preview_navigation();
    }

    /// P6: move the armed gizmo drag (if any) to the pointer. The gesture
    /// scrubs each param's knob, so the write is the live, non-undoable one
    /// the knob makes. Returns `true` if a drag was armed.
    fn editor_viewport_gizmo_drag_move(&mut self, x: f32, y: f32) -> bool {
        let Some(ed) = self.graph_editor.as_mut() else {
            return false;
        };
        let Some(mut gesture) = ed.viewport_gizmo_drag.take() else {
            return false;
        };
        let point = ed
            .viewport_rect
            .zip(
                ed.viewport_session
                    .as_ref()
                    .and_then(|session| session.displayed_config()),
            )
            .and_then(|(rect, config)| {
                let view = crate::scene_gizmo::GizmoView::new(
                    config.camera.to_camera(),
                    config.width,
                    config.height,
                );
                crate::viewport_input::gizmo_pointer(rect, view, (x, y))
            });
        if let Some((view, point)) = point {
            self.with_dispatch_ctx(|ctx| gesture.drag_to(ctx, &view, point));
        }
        if let Some(ed) = self.graph_editor.as_mut() {
            ed.viewport_gizmo_drag = Some(gesture);
        }
        true
    }

    /// End a gizmo drag: its scrub commits one undo entry.
    pub(crate) fn end_gizmo_gesture(&mut self, gesture: Option<crate::scene_gizmo::GizmoGesture>) {
        if let Some(gesture) = gesture {
            self.with_dispatch_ctx(|ctx| gesture.commit(ctx));
        }
    }

    /// `MouseInput` in the editor window, through the editor-input seam. A
    /// left press the seam hands back from the 3D viewport tries a gizmo grab
    /// or object pick, else orbits.
    pub(crate) fn editor_mouse_input(
        &mut self,
        window_id: WindowId,
        button: MouseButton,
        state: ElementState,
    ) {
        let Some(mut cx) = self.editor_pointer(window_id) else {
            return;
        };
        let out = crate::editor_input::button(&mut cx, button, state);
        self.end_gizmo_gesture(out.gizmo_release);
        if let Some(pos) = out.viewport_press
            && !self.editor_viewport_gizmo_press(pos.x, pos.y)
            && let Some(ws) = self.graph_editor.as_mut()
        {
            crate::editor_input::arm_viewport_drag(ws, button, pos);
        }
    }

    /// `MouseWheel` in the editor window. False when no canvas took it.
    pub(crate) fn editor_mouse_wheel(
        &mut self,
        window_id: WindowId,
        delta: MouseScrollDelta,
    ) -> bool {
        self.editor_pointer(window_id)
            .is_some_and(|mut cx| crate::editor_input::wheel(&mut cx, delta))
    }

    /// Pinch in the editor window zooms the graph canvas around the cursor.
    fn editor_pinch(&mut self, window_id: WindowId, magnification: f32) {
        if let Some(mut cx) = self.editor_pointer(window_id) {
            crate::editor_input::pinch(&mut cx, magnification);
        }
    }

    /// All editor-window keyboard handling — popover/canvas text-field editing,
    /// node-picker filter, graph text fields, node delete, scope nav, and the
    /// editor shortcuts (group/ungroup/tidy/tint/rename/find/copy/paste/dump/
    /// undo). Returns `true` when consumed (the old branches all `return`ed).
    /// `is_graph_editor` is threaded in because the leading popover-editing
    /// block was ungated (its `is_editing()` guard only fires in the editor),
    /// while the rest gate on the focused window.
    pub(crate) fn editor_keyboard_input(
        &mut self,
        is_graph_editor: bool,
        logical_key: Key,
    ) -> bool {
        if crate::editor_input::popover_key(
            &mut self.editor_mapping_popover,
            self.graph_canvas.as_mut(),
            crate::text_session::TextKey::from_winit(&logical_key),
            self.modifiers,
        ) {
            return true;
        }
        if is_graph_editor && matches!(logical_key, Key::Named(NamedKey::Escape)) {
            let capture_cancelled = self
                .graph_editor_window_id
                .map(|window_id| self.cancel_editor_pointer_capture(window_id))
                .unwrap_or(false);
            let picker_open = self
                .graph_editor
                .as_ref()
                .is_some_and(|ed| ed.ui_root.browser_popup.is_open());
            if capture_cancelled && !picker_open {
                return true;
            }
        }
        // Editor window: node picker is open → keystrokes drive its
        // search field. Must run BEFORE the Delete/Backspace node-
        // delete arm below, or Backspace would delete a node instead
        // of editing the filter. Escape dismisses; chars/space type.
        // Each moves the overlays' version (filter, cursor) or open set
        // (close), which the editor's plan rebuilds and composites.
        if is_graph_editor
            && self
                .graph_editor
                .as_ref()
                .is_some_and(|ed| ed.ui_root.browser_popup.is_open())
        {
            use winit::keyboard::{Key, NamedKey};
            let mut filter_changed = false;
            match &logical_key {
                Key::Named(NamedKey::Escape) => {
                    // Bypasses `route_overlay_event` (this key path is bespoke
                    // to the editor's browser popup), so record the close
                    // ourselves — the per-frame pump in `tick_and_render`
                    // drains it and cancels this popup's owned text session.
                    // Guarded by the outer `is_open()` check above, so the
                    // popup is open here.
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root.browser_popup.handle_escape();
                        ed.ui_root
                            .note_overlay_closed_if(crate::ui_root::OverlayId::BrowserPopup, true);
                    }
                }
                Key::Named(NamedKey::Backspace) => {
                    self.text_input.backspace();
                    filter_changed = true;
                }
                Key::Named(NamedKey::Space) => {
                    self.text_input.insert_char(' ');
                    filter_changed = true;
                }
                Key::Character(c) => {
                    for ch in c.chars() {
                        self.text_input.insert_char(ch);
                    }
                    filter_changed = true;
                }
                // Picker keyboard nav (P2): arrows move the grid cursor,
                // which the popup draws when it builds.
                Key::Named(NamedKey::ArrowUp) => {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root
                            .browser_popup
                            .handle_key_nav(manifold_ui::input::Key::Up);
                        ed.ui_root.mark_overlay_dirty();
                    }
                }
                Key::Named(NamedKey::ArrowDown) => {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root
                            .browser_popup
                            .handle_key_nav(manifold_ui::input::Key::Down);
                        ed.ui_root.mark_overlay_dirty();
                    }
                }
                // Enter picks (type-and-enter fast path when no cursor and a
                // non-empty filter). A pick closes the popup — bypasses
                // `route_overlay_event` same as Escape above, so record the
                // close for the closed-overlay pump ourselves.
                Key::Named(NamedKey::Enter) => {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        let was_open = ed.ui_root.browser_popup.is_open();
                        if let Some(action) = ed
                            .ui_root
                            .browser_popup
                            .handle_key_nav(manifold_ui::input::Key::Enter)
                        {
                            ed.ui_root.note_overlay_closed_if(
                                crate::ui_root::OverlayId::BrowserPopup,
                                was_open,
                            );
                            use manifold_ui::panels::browser_popup::BrowserPopupAction;
                            if let BrowserPopupAction::NodeSelected { type_id, graph_pos } = action
                                && let Some(canvas) = self.graph_canvas.as_mut()
                            {
                                canvas.request_add_node_at(type_id, graph_pos);
                            }
                        }
                    }
                }
                // Suppress every other key while the modal is up.
                _ => {}
            }
            if filter_changed {
                let filter = self.text_input.text().trim().to_string();
                if let Some(ed) = self.graph_editor.as_mut() {
                    ed.ui_root.set_browser_filter(filter);
                }
            }
            return true;
        }
        // Editor window: a graph text field (group rename / String param /
        // wgsl / node search) is active → keystrokes type into it. Ahead
        // of node-delete + navigation so Backspace edits text and Esc
        // cancels the edit rather than deleting a node or leaving a group.
        if is_graph_editor && self.text_input.active && self.text_input.field.is_graph_field() {
            use winit::keyboard::{Key, NamedKey};
            let typing = !self.modifiers.command && !self.modifiers.ctrl;
            let was_search =
                self.text_input.field == crate::text_input::TextInputField::GraphNodeSearch;
            match &logical_key {
                Key::Named(NamedKey::Escape) => {
                    if let Some(window_id) = self.graph_editor_window_id {
                        self.cancel_editor_pointer_capture(window_id);
                    }
                    self.text_input.cancel();
                    if was_search && let Some(canvas) = self.graph_canvas.as_mut() {
                        canvas.set_node_search("");
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    // Same rule as the main window: Shift+Enter is a new
                    // line in a multiline field, Enter commits.
                    if self.text_input.multiline && self.modifiers.shift {
                        self.text_input.insert_char('\n');
                    } else {
                        let (field, text) = self.text_input.commit();
                        self.handle_text_input_commit(field, &text);
                    }
                }
                Key::Named(NamedKey::Backspace) => self.text_input.backspace(),
                Key::Named(NamedKey::Delete) => self.text_input.delete(),
                Key::Named(
                    k @ (NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown),
                ) => self.text_input_arrow(*k),
                Key::Named(NamedKey::Space) if typing => self.text_input.insert_char(' '),
                Key::Character(c) => {
                    if c == "a" && self.modifiers.command {
                        self.text_input.select_all_text();
                    } else if c == "z" && self.modifiers.command {
                        self.text_input.undo_to_seed();
                    } else if c == "c" && self.modifiers.command {
                        let s = self.text_input.copy_selection();
                        if !s.is_empty() {
                            crate::macos_pasteboard::set_general_pasteboard_string(&s);
                        }
                    } else if c == "x" && self.modifiers.command {
                        let s = self.text_input.cut_selection();
                        if !s.is_empty() {
                            crate::macos_pasteboard::set_general_pasteboard_string(&s);
                        }
                    } else if c == "v" && self.modifiers.command {
                        if let Some(s) = crate::macos_pasteboard::general_pasteboard_string() {
                            self.text_input.paste(&s);
                        }
                    } else if typing {
                        for ch in c.chars() {
                            self.text_input.insert_char(ch);
                        }
                    }
                }
                _ => {}
            }
            // Live node-search highlight: push the query each keystroke
            // while the search field is still active.
            if was_search
                && self.text_input.active
                && let Some(canvas) = self.graph_canvas.as_mut()
            {
                let q = self.text_input.text().trim().to_string();
                canvas.set_node_search(&q);
            }
            // The open field is a live surface of the editor's composite, and
            // its close moves the composite key.
            return true;
        }
        // Editor window: Delete/Backspace removes the currently
        // selected node. Has to happen before primary keyboard
        // routing because the editor window has its own focus
        // semantics.
        if is_graph_editor
            && matches!(
                logical_key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Delete)
                    | winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace)
            )
        {
            if let Some(canvas) = self.graph_canvas.as_mut() {
                canvas.request_delete_selected();
            }
            return true;
        }
        // Editor window: graph-canvas navigation + debug + transport keys.
        //   Space → toggle play/pause (preview motion while authoring), same
        //           as the mini-timeline play button. Reached only when no text
        //           field / popover is active (those arms returned above).
        //   Esc → leave the current group (pop one scope level), if
        //         the view is inside a group.
        //   `   → toggle the graph debug overlay; Shift+` opens the
        //         performance HUD without changing that existing shortcut.
        //   v   → toggle the P5c 3D viewport (`docs/REALTIME_3D_DESIGN.md`).
        //         Only takes effect once the previewed node is a top-level
        //         `node.render_scene` node (`app_render.rs`'s
        //         `present_graph_editor_window` gates the actual session on
        //         that + this bool) — pressing it otherwise just arms the
        //         toggle for when a qualifying node is selected.
        // (Ctrl+G group / Ctrl+Shift+G ungroup are handled below.)
        if is_graph_editor {
            use winit::keyboard::{Key, NamedKey};
            let mut handled = false;
            match &logical_key {
                Key::Named(NamedKey::Space) => {
                    let cmd = if self.content_state.is_playing {
                        ContentCommand::Pause
                    } else {
                        ContentCommand::Play
                    };
                    self.send_content_cmd(cmd);
                    handled = true;
                }
                Key::Named(NamedKey::Escape) => {
                    if let Some(canvas) = self.graph_canvas.as_mut() {
                        handled = canvas.exit_group();
                    }
                }
                // Preserve the editor canvas' existing debug overlay shortcut
                // on bare backtick. Shift+backtick opens the performance HUD,
                // so the new presentation surface does not steal the debug
                // command that graph editing already exposed.
                Key::Character(c)
                    if matches!(c.as_str(), "`" | "~") && self.modifiers.is_shift_only() =>
                {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root.perf_hud.toggle();
                    }
                    handled = true;
                }
                Key::Character(c) if c.as_str() == "`" && self.modifiers.is_none() => {
                    if let Some(canvas) = self.graph_canvas.as_mut() {
                        canvas.toggle_debug_overlay();
                    }
                    handled = true;
                }
                Key::Character(c) if c.eq_ignore_ascii_case("v") => {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.viewport_open = !ed.viewport_open;
                        ed.viewport_drag = None;
                    }
                    let gesture = self
                        .graph_editor
                        .as_mut()
                        .and_then(|ed| ed.viewport_gizmo_drag.take());
                    self.end_gizmo_gesture(gesture);
                    if let Some(ed) = self.graph_editor.as_mut()
                        && !ed.viewport_open
                    {
                        // Stop sending viewport requests immediately; the
                        // content thread owns rendering resources.
                        ed.viewport_session = None;
                        ed.viewport_rect = None;
                    }
                    handled = true;
                }
                // P6 (`docs/REALTIME_3D_DESIGN.md` D7 "industry-standard
                // viewport bindings"): W/E/R cycles the gizmo mode
                // move/rotate/scale — the Maya convention — only while the
                // viewport is actually open (these letters are otherwise
                // unbound in the editor, but scoping to "viewport open"
                // keeps the door closed for a future collision).
                Key::Character(c)
                    if self
                        .graph_editor
                        .as_ref()
                        .is_some_and(|ed| ed.viewport_open)
                        && (c.eq_ignore_ascii_case("w")
                            || c.eq_ignore_ascii_case("e")
                            || c.eq_ignore_ascii_case("r")) =>
                {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.viewport_gizmo_mode = if c.eq_ignore_ascii_case("w") {
                            manifold_nodes_scene::node_graph::viewport_gizmo::GizmoMode::Move
                        } else if c.eq_ignore_ascii_case("e") {
                            manifold_nodes_scene::node_graph::viewport_gizmo::GizmoMode::Rotate
                        } else {
                            manifold_nodes_scene::node_graph::viewport_gizmo::GizmoMode::Scale
                        };
                    }
                    handled = true;
                }
                _ => {}
            }
            // Each lands through an input the composite reads: the play
            // state, the canvas's view version, the overlay open set, the
            // viewport image.
            if handled {
                return true;
            }
        }
        // Editor window: Cmd+G collapses the canvas selection into a
        // group; Cmd+Shift+G dissolves a selected group. The primary
        // window's Cmd+Shift+G (open editor) and Cmd+G (group layers)
        // are gated on `is_primary`, so there's no clash — the editor
        // window intercepts here and returns before either fires.
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("g")
        {
            let shift = self.modifiers.shift;
            if let Some(canvas) = self.graph_canvas.as_mut() {
                if shift {
                    canvas.request_ungroup();
                } else {
                    canvas.request_group_selection();
                }
            }
            return true;
        }
        // Editor window: Cmd+L tidies the current level — re-runs the
        // layered auto-layout and persists every node's new position in
        // one undoable step.
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("l")
        {
            if let Some(canvas) = self.graph_canvas.as_mut() {
                canvas.request_relayout();
            }
            return true;
        }
        // Editor window: Cmd+T recolours the selected group, cycling its
        // header through the preset tint palette for at-a-glance legibility.
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("t")
        {
            if let Some(canvas) = self.graph_canvas.as_mut() {
                canvas.request_cycle_group_tint();
            }
            return true;
        }
        // Editor window: F2 renames the single selected group inline.
        // The rename field anchors over the group's header; commit
        // routes to RenameGroupCommand at the canvas scope.
        if is_graph_editor
            && matches!(
                logical_key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::F2)
            )
        {
            let target = self
                .editor_canvas_viewport()
                .zip(self.graph_canvas.as_ref())
                .and_then(|(vp, canvas)| canvas.group_rename_target(vp));
            if let Some((gid, name, sx, sy, sw, sh)) = target {
                self.text_input.begin(
                    crate::text_input::TextInputField::GraphGroupRename(gid),
                    &name,
                    crate::text_input::AnchorRect::new(sx, sy, sw.max(80.0), sh.max(18.0)),
                    13.0,
                );
            }
            return true;
        }
        // Editor window: Cmd+F opens the find-a-node search box. Typing
        // dims every non-matching node live; Esc clears, Enter keeps the
        // highlight. Anchored top-left over the canvas.
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("f")
        {
            let anchor = self
                .editor_canvas_viewport()
                .map(|vp| crate::text_input::AnchorRect::new(vp.x + 12.0, 12.0, 220.0, 18.0))
                .unwrap_or_else(|| crate::text_input::AnchorRect::new(120.0, 12.0, 220.0, 18.0));
            let initial = self
                .graph_canvas
                .as_ref()
                .map(|c| c.node_search().to_string())
                .unwrap_or_default();
            self.text_input.begin(
                crate::text_input::TextInputField::GraphNodeSearch,
                &initial,
                anchor,
                13.0,
            );
            return true;
        }
        // Editor window: Cmd+C copies the selected nodes (and the wires
        // among them) to the graph clipboard; Cmd+V pastes them offset
        // with fresh ids. Duplicate is just copy-then-paste. (Cmd+D is
        // taken by the node-dump below.)
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("c")
        {
            self.graph_node_clipboard = self.copy_selected_graph_nodes();
            return true;
        }
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("v")
        {
            if let (Some(target), Some(default), Some((nodes, wires))) = (
                self.watched_graph_target.clone(),
                self.watched_catalog_default.clone(),
                self.graph_node_clipboard.clone(),
            ) {
                let scope = self
                    .graph_canvas
                    .as_ref()
                    .map(|c| c.scope_path().to_vec())
                    .unwrap_or_default();
                let cmd = manifold_editing::commands::graph::PasteNodesCommand::new(
                    target,
                    scope,
                    nodes,
                    wires,
                    (30.0, 30.0),
                    default,
                );
                // The pasted nodes arrive in the next graph snapshot, which
                // the canvas feed draws.
                self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
            }
            return true;
        }
        // Editor window: Cmd+D dumps every node output of the watched
        // effect to a temp folder (16-bit PNGs + manifest) for visual
        // inspection. The content thread picks the dir and logs it.
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("d")
        {
            if let Some(tx) = self.content_tx.as_ref() {
                crate::content_command::ContentCommand::send(
                    tx,
                    crate::content_command::ContentCommand::DumpGraphOutputs,
                );
            }
            return true;
        }
        // Editor window: Cmd+Z / Cmd+Shift+Z route to the
        // content thread's undo stack so graph edits can be
        // reverted while the editor has focus. (The primary
        // window's InputHandler covers Cmd+Z when its focused
        // — but the editor has its own keyboard routing that
        // returns before InputHandler fires, so we wire the
        // shortcut here too.)
        if is_graph_editor
            && self.modifiers.command
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("z")
        {
            self.selection.clear_automation_selection();
            if let Some(tx) = self.content_tx.as_ref() {
                if self.modifiers.shift {
                    crate::ui_bridge::redo(tx);
                } else {
                    crate::ui_bridge::undo(tx);
                }
            }
            // The undone graph arrives in the next snapshot, which the canvas
            // feed draws.
            return true;
        }
        false
    }

    /// `Resized` in the editor window: resize the surface + offscreen target.
    pub(crate) fn editor_resized(&mut self, window_id: WindowId, size: PhysicalSize<u32>) {
        if let Some(ws) = self.window_registry.get_mut(&window_id)
            && let Some(surface) = &mut ws.surface
        {
            surface.resize(size.width.max(1), size.height.max(1));
        }
        // The new target resets the composite key, so the next pulse draws it.
        self.resize_graph_editor_offscreen(size.width.max(1), size.height.max(1));
        if let Some(ed) = self.graph_editor.as_mut() {
            ed.surface_resized_this_frame = true;
        }
    }

    /// `ScaleFactorChanged` in the editor window: re-resize from the new
    /// physical inner size (scale_factor itself is unused — the inner size
    /// already reflects it).
    pub(crate) fn editor_scale_factor_changed(&mut self, window_id: WindowId, scale_factor: f64) {
        let mut new_size = (1u32, 1u32);
        if let Some(ws) = self.window_registry.get_mut(&window_id)
            && let Some(surface) = &mut ws.surface
        {
            let size = ws.window.inner_size();
            new_size = (size.width.max(1), size.height.max(1));
            surface.resize(new_size.0, new_size.1);
        }
        self.resize_graph_editor_offscreen(new_size.0, new_size.1);
        if let Some(ed) = self.graph_editor.as_mut() {
            ed.surface_resized_this_frame = true;
        }
        let _ = scale_factor;
    }

    /// `KeyboardInput` (a `Pressed` key) for any window. This is the one keyboard
    /// owner: perform-mode handoff, then the editor's keyboard ([`Self::
    /// editor_keyboard_input`], called unconditionally because its leading
    /// popover-editing block is ungated), then the open-editor chord, then the
    /// primary window's text-input + `InputHandler` shortcuts + file ops + the
    /// UI-forward. Each `return` ends the keystroke; ordering is exactly the old
    /// `window_event` arm's, by focused window.
    pub(crate) fn input_keyboard(
        &mut self,
        is_primary: bool,
        is_graph_editor: bool,
        logical_key: Key,
    ) {
        if self.ws.ui_root.export_progress.is_open() {
            if matches!(logical_key, Key::Named(NamedKey::Escape))
                && self.ws.ui_root.export_progress.request_cancel()
            {
                self.send_content_cmd(crate::content_command::ContentCommand::CancelExport);
                self.ws.ui_root.mark_overlay_dirty();
            }
            return;
        }
        if is_primary && self.perform_handle_key(&logical_key) {
            return;
        }
        // Editor-window keyboard handling — popover/canvas text-field
        // editing, node-picker filter, graph text fields, node delete,
        // scope nav, and the editor shortcuts. Called unconditionally
        // because the leading popover-editing block is ungated (its
        // `is_editing` guard only fires in the editor); the rest gate on
        // `is_graph_editor`.
        if self.editor_keyboard_input(is_graph_editor, logical_key.clone()) {
            return;
        }
        // Cmd+Shift+G — open the node-graph editor window.
        // App-level shortcut, fires before text input or UI
        // forwarding so it's always available regardless of
        // focus.
        if is_primary
            && self.modifiers.command
            && self.modifiers.shift
            && let winit::keyboard::Key::Character(c) = &logical_key
            && c.eq_ignore_ascii_case("g")
        {
            self.pending_open_graph_editor = true;
            return;
        }
        // App-level shortcuts (handled before UI forwarding)
        let mut consumed = false;
        if is_primary {
            // Escape cancels an in-flight timeline clip or automation drag before any
            // other Escape handling — a live drag holds uncommitted model
            // mutations (`InteractionOverlay::cancel_drag`, P1.4 D5/B8:
            // "restore and close batch", never commit-then-undo) that must
            // be rolled back before anything else touches the project.
            if matches!(logical_key, Key::Named(NamedKey::Escape))
                && (self.overlay.has_pending_automation_press()
                    || matches!(
                        self.overlay.drag_mode(),
                        DragMode::Move
                            | DragMode::TrimLeft
                            | DragMode::TrimRight
                            | DragMode::AutomationPoint
                            | DragMode::AutomationSegmentBend
                            | DragMode::AutomationSegmentDrag
                            | DragMode::AutomationMarquee
                            | DragMode::AutomationGroupMove
                            | DragMode::AutomationDraw
                    ))
                && let Some(content_tx) = self.content_tx.as_ref()
            {
                let automation_cancel = self.overlay.has_pending_automation_press()
                    || matches!(
                        self.overlay.drag_mode(),
                        DragMode::AutomationPoint
                            | DragMode::AutomationSegmentBend
                            | DragMode::AutomationSegmentDrag
                            | DragMode::AutomationMarquee
                            | DragMode::AutomationGroupMove
                            | DragMode::AutomationDraw
                    );
                let mut host = crate::editing_host::AppEditingHost::new(
                    self.local_project.edit(&mut self.ui_edits, content_tx),
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.ws.offscreen_dirty,
                );
                self.overlay.cancel_drag(&mut host);
                if automation_cancel {
                    self.selection.clear_automation_selection();
                }
                return;
            }

            // An active text session takes every key (`text_session.rs`).
            match crate::text_session::main_window_key(
                &mut self.text_input,
                &mut self.ws.ui_root,
                crate::text_session::TextKey::from_winit(&logical_key),
                self.modifiers,
                self.ui_renderer.is_some(),
            ) {
                crate::text_session::KeyOutcome::NotTyping => {}
                crate::text_session::KeyOutcome::Consumed
                | crate::text_session::KeyOutcome::Filtered => return,
                crate::text_session::KeyOutcome::Committed(field, text) => {
                    self.handle_text_input_commit(field, &text);
                    return;
                }
            }
            // The preview: Escape ends a gizmo drag, else navigation; with
            // the preview focused, W, E and R pick the mode, as in the
            // editor viewport.
            if matches!(logical_key, Key::Named(NamedKey::Escape))
                && (self.preview_gizmo_release() || self.end_preview_navigation())
            {
                return;
            }
            if self.modifiers.is_none()
                && self.ws.ui_root.scene_setup_panel.gizmo_layer().is_some()
                && let Key::Character(c) = &logical_key
                && self.ws.preview_gizmo.mode_key(c.as_str())
            {
                return;
            }
            // ── Shortcut dispatch via InputHandler ──
            // Port of Unity InputHandler.HandleKeyboardInput().
            // All viewport access goes through the TimelineInputHost trait.
            if !consumed && let Some(content_tx) = self.content_tx.as_ref() {
                let mut host = crate::input_host::AppInputHost {
                    project: self.local_project.edit(&mut self.ui_edits, content_tx),
                    content_tx,
                    content_state: &self.content_state,
                    ui_root: &mut self.ws.ui_root,
                    selection: &mut self.selection,
                    active_layer: &mut self.active_layer_id,
                    redraw: &mut self.ws.offscreen_dirty,
                    current_project_path: &self.current_project_path,
                    has_output_window: self.window_registry.has_output_window(),
                    pending_close_output: &mut self.pending_close_output,
                    pending_export: &mut self.pending_export,
                    project_io: &mut self.project_io,
                    import_failures_rx: &mut self.import_failures_rx,
                    #[cfg(target_os = "macos")]
                    internal_clipboard_change_count: &mut self.internal_clipboard_change_count,
                };
                if self
                    .input_handler
                    .handle_keyboard_input(&logical_key, self.modifiers, &mut host)
                {
                    consumed = true;
                }
            }

            // File operations: Save/Open/New require rfd dialogs and window
            // handles not available to AppInputHost. InputHandler returns false
            // for these, so they fall through here.
            if !consumed {
                let m = self.modifiers;
                match &logical_key {
                    // ── Save: Cmd+S ──
                    Key::Character(c) if c.as_str() == "s" && m.is_command_only() => {
                        self.save_project();
                        consumed = true;
                    }
                    // ── Open: Cmd+O ──
                    Key::Character(c) if c.as_str() == "o" && m.is_command_only() => {
                        self.open_project();
                        consumed = true;
                    }
                    // ── Import Video: Cmd+I ──
                    Key::Character(c) if c.as_str() == "i" && m.is_command_only() => {
                        self.import_video_clip();
                        consumed = true;
                    }
                    // ── New: Cmd+N ──
                    Key::Character(c) if c.as_str() == "n" && m.is_command_only() => {
                        let project = Self::create_default_project();
                        self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));
                        self.send_content_cmd(ContentCommand::SetProject);
                        log::info!("New project created");
                        consumed = true;
                    }

                    _ => {}
                }
            } // end if !consumed (file operations)
        } // end if is_primary

        // All other shortcuts handled by InputHandler → AppInputHost.

        // Forward to UI input system (unless consumed by app shortcut)
        if is_primary
            && !consumed
            && let Some(ui_key) = Self::convert_key(&logical_key)
        {
            self.ws.ui_root.key_event(ui_key, self.modifiers);
        }

        // Output window: Escape no longer closes it — during a live
        // show an accidental Escape would black out the audience.
        // Close via the UI Monitor button instead.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    #[test]
    fn font_picker_receives_native_wheel_and_trackpad_scroll() {
        use manifold_ui::node::Rect;
        use manifold_ui::panels::browser_popup::ActionListOptions;

        let mut app = Application::new();
        let labels: Vec<String> = (0..100).map(|i| format!("Font {i:03}")).collect();
        let actions = labels
            .iter()
            .map(|label| {
                manifold_ui::PanelAction::Params(manifold_ui::ParamsAction::GenStringParamSelected(
                    0,
                    label.clone(),
                ))
            })
            .collect();
        app.ws.ui_root.open_action_list(
            labels,
            actions,
            ActionListOptions {
                empty_label: "No fonts match",
                label_in_own_font: true,
                current: Some(0),
                ..Default::default()
            },
            Rect::new(100.0, 100.0, 100.0, 30.0),
        );
        app.ws.ui_root.build_overlays_for_screen(1280.0, 800.0);
        let viewport = app
            .ws
            .ui_root
            .browser_popup
            .picker()
            .unwrap()
            .scroll
            .viewport();
        app.cursor_pos = Vec2::new(viewport.x + 20.0, viewport.y + 20.0);
        let timeline_y = app.ws.ui_root.viewport.scroll_y_px();
        for delta in [
            MouseScrollDelta::LineDelta(0.0, -3.0),
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -45.0)),
        ] {
            let before = app
                .ws
                .ui_root
                .browser_popup
                .picker()
                .unwrap()
                .scroll
                .scroll_offset();
            let version = app.ws.ui_root.overlays_version();
            app.primary_mouse_wheel(true, delta);
            let events = app.ws.ui_root.input.drain_events();
            assert_eq!(events.len(), 1, "native scrolling must reach the popup");
            let mut actions = Vec::new();
            assert!(app.ws.ui_root.route_overlay_event(&events[0], &mut actions));
            assert!(actions.is_empty());
            assert_ne!(
                app.ws.ui_root.overlays_version(),
                version,
                "scrolling must rebuild the overlays"
            );
            app.ws.ui_root.tree.clear();
            app.ws.ui_root.build_overlays_for_screen(1280.0, 800.0);
            assert!(
                app.ws
                    .ui_root
                    .browser_popup
                    .picker()
                    .unwrap()
                    .scroll
                    .scroll_offset()
                    > before
            );
            assert_eq!(app.ws.ui_root.viewport.scroll_y_px(), timeline_y);
        }
    }

    // Pins the scroll-normalization rule the three former call sites (primary
    // scroll, open-dropdown scroll, editor zoom) now share. A regression here
    // would silently change wheel speed in one window but not the other — the
    // exact "two parallel loops" drift Phase 7 removed.
    #[test]
    fn line_delta_scales_by_notch_pixels() {
        let (dx, dy) = normalize_scroll_delta(MouseScrollDelta::LineDelta(2.0, -3.0));
        assert_eq!(dx, 2.0 * LINE_DELTA_PX);
        assert_eq!(dy, -3.0 * LINE_DELTA_PX);
    }

    #[test]
    fn pixel_delta_passes_through() {
        let (dx, dy) = normalize_scroll_delta(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
            7.5, -4.25,
        )));
        assert_eq!(dx, 7.5);
        assert_eq!(dy, -4.25);
    }
}
