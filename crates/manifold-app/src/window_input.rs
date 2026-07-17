//! The window input owner (Phase 7 — "one shared input owner for both windows").
//!
//! Both the timeline/inspector window and the graph-editor window enter their
//! pointer / scroll / keyboard handling through the `input_*` dispatchers here.
//! `App::window_event` is a thin router: each input arm is one delegation into
//! this module, and the `is_graph_editor` / `is_primary` branching lives in the
//! dispatcher — not smeared across the match. This replaces the two parallel
//! event *policies* the audit flagged ("two parallel event loops"): the primary
//! window's bodies used to be inlined in `window_event`, the editor's in a
//! separate `editor_input.rs`. Now there is one owner.
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
//! Behaviour-preserving: the moved bodies are verbatim. The editor's
//! viewport-slice math (`palette_width`/`sidebar_x`) and the explicit
//! `offscreen_dirty` marking (the editor has no idle repaint loop) are preserved
//! exactly — losing either would mis-hit-test or freeze the editor.

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

impl Application {
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
        Vec2::new(position.x as f32 / scale as f32, position.y as f32 / scale as f32)
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
            let icon = match self.cursor_manager.pending_cursor_icon() {
                TimelineCursor::Default => winit::window::CursorIcon::Default,
                TimelineCursor::ResizeHorizontal => winit::window::CursorIcon::ColResize,
                TimelineCursor::ResizeVertical => winit::window::CursorIcon::RowResize,
                TimelineCursor::Move => winit::window::CursorIcon::Move,
                TimelineCursor::Blocked => winit::window::CursorIcon::NotAllowed,
            };
            ws.window.set_cursor(icon);
            self.cursor_manager.mark_applied();
        }
    }

    // ── Text-edit session pointer routing (UI_WIDGET_UNIFICATION P5b/D16) ──
    // A single `TextInputState` is shared by both windows (`begin_owned`
    // tags which one), so one gate here covers both — mirrors the keyboard
    // gate's shape (`self.text_input.active`) rather than duplicating a
    // second one per window.

    /// Approximate rendered rect of the active text overlay, in the same
    /// logical-pixel space as `self.cursor_pos` — mirrors
    /// `app_render::render_text_input_overlay`'s own `bg_w`/`bg_h` sizing
    /// (single-line fields exactly; multiline uses the anchor height, a
    /// reasonable approximation — precise multiline geometry needs the live
    /// line count, which only the renderer computes today).
    fn text_input_overlay_rect(&self) -> (f32, f32, f32, f32) {
        let a = self.text_input.anchor;
        let bg_w = a.width.max(40.0);
        let bg_h = a
            .height
            .max(self.text_input.font_size + crate::text_input::TEXT_INPUT_PAD_V * 2.0);
        (a.x, a.y, bg_w, bg_h)
    }

    /// Byte offset under `x` (logical px, relative to the anchor's left
    /// edge) via the live `UIRenderer`'s own measurer — the same
    /// `byte_offset_for_x` helper `x_for_byte_offset` inverts for rendering.
    /// Falls back to the text's end when no renderer is up yet (can't
    /// happen once a session is active in practice, since the overlay that
    /// hosts it needs the renderer to draw — belt and suspenders).
    fn text_input_byte_at_x(&mut self, x: f32) -> usize {
        let pad_h = crate::text_input::TEXT_INPUT_PAD_H;
        let rel_x = x - self.text_input.anchor.x - pad_h;
        let fs = self.text_input.font_size as u16;
        let text = self.text_input.text().to_string();
        match self.ui_renderer.as_mut() {
            Some(r) => manifold_ui::text_edit::byte_offset_for_x(&text, rel_x, &mut |s| {
                r.measure_text_cached(s, fs, manifold_ui::FontWeight::Medium).x
            }),
            None => text.len(),
        }
    }

    /// A left-press anywhere while a text session is active. Inside the
    /// field: places the caret (shift-click extends; a second press within
    /// the double-click window/radius, I8, selects the word) and starts a
    /// drag session — returns `true` (consumed, no further dispatch).
    /// Outside the field: commits the session first (D16 blur-commit), then
    /// returns `false` so the ORIGINAL press still reaches its normal
    /// target — a click that both closes a rename box and clicks whatever
    /// is under it.
    pub(crate) fn text_input_pointer_down(&mut self, pos: Vec2) -> bool {
        if !self.text_input.active {
            return false;
        }
        let (rx, ry, rw, rh) = self.text_input_overlay_rect();
        let inside = pos.x >= rx && pos.x <= rx + rw && pos.y >= ry && pos.y <= ry + rh;
        if !inside {
            let (field, text) = self.text_input.commit();
            self.handle_text_input_commit(field, &text);
            return false;
        }
        let byte = self.text_input_byte_at_x(pos.x);
        let now = self.time_since_start;
        let is_double_click = self.text_input.last_press.is_some_and(|(t, px, py)| {
            (now - t) < manifold_ui::color::DOUBLE_CLICK_TIME_SEC
                && (pos.x - px).powi(2) + (pos.y - py).powi(2)
                    < manifold_ui::color::DOUBLE_CLICK_RADIUS_PX.powi(2)
        });
        self.text_input.last_press = Some((now, pos.x, pos.y));
        if is_double_click {
            self.text_input.select_word_at(byte);
        } else {
            self.text_input.caret_to(byte, self.modifiers.shift);
        }
        self.text_input.dragging = true;
        true
    }

    /// Pointer motion while a text session's drag is armed — extends the
    /// selection toward the cursor (`drag_to`, anchor held at the press
    /// position). A no-op (returns `false`) when no drag is in progress, so
    /// the caller can fall through to normal hover handling.
    pub(crate) fn text_input_pointer_move(&mut self, pos: Vec2) -> bool {
        if !self.text_input.active || !self.text_input.dragging {
            return false;
        }
        let byte = self.text_input_byte_at_x(pos.x);
        self.text_input.drag_to(byte);
        true
    }

    /// Ends a text-session drag on release. Idempotent — a no-op if no drag
    /// was in progress (e.g. the button went up outside a session).
    pub(crate) fn text_input_pointer_up(&mut self) {
        self.text_input.dragging = false;
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

        // Split handle drag takes highest priority
        // From Unity PanelResizeHandle.OnDrag
        if self.split_dragging {
            self.ws
                .ui_root
                .layout
                .update_split_from_drag(self.cursor_pos.y);
            self.cursor_manager.set(TimelineCursor::ResizeVertical);
            self.needs_rebuild = true;
        }
        // Inspector resize drag takes next priority
        else if self.ws.ui_root.inspector_resize_dragging {
            if self.ws.ui_root.update_inspector_resize(self.cursor_pos.x) {
                self.needs_rebuild = true;
            }
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
        }
        // Audio Setup dock resize drag (D1)
        else if self.ws.ui_root.audio_setup_resize_dragging {
            if self.ws.ui_root.update_audio_setup_resize(self.cursor_pos.x) {
                self.needs_rebuild = true;
            }
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
        }
        // Scene Setup dock resize drag — mirror of the Audio Setup one above
        // (SCENE_SETUP_PANEL_DESIGN D2).
        else if self.ws.ui_root.scene_setup_resize_dragging {
            if self.ws.ui_root.update_scene_setup_resize(self.cursor_pos.x) {
                self.needs_rebuild = true;
            }
            self.cursor_manager.set(TimelineCursor::ResizeHorizontal);
        } else {
            self.ws.ui_root.pointer_event(
                self.cursor_pos,
                PointerAction::Move,
                self.time_since_start,
            );

            // Route hover through InteractionOverlay (port of Unity OnPointerMove).
            // This handles: CursorBeat/CursorLayerIndex tracking, per-layer bitmap
            // invalidation on hover change, and cursor shape feedback.
            if let Some(content_tx) = self.content_tx.as_ref() {
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.scroll_dirty,
                    &mut self.invalidate_layers,
                    &mut self.pre_drag_commands,
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

        self.apply_pending_cursor(window_id);
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

                            // Track which panel has focus for context-sensitive shortcuts.
                            // Matches Unity's InputHandler.inspectorHasFocus.
                            // Any click outside inspector clears focus and effect selection
                            // — layer headers, timeline tracks, transport bar, etc.
                            let inspector_rect = self.ws.ui_root.layout.inspector();
                            let in_inspector = inspector_rect.contains(self.cursor_pos);
                            if !in_inspector && self.input_handler.inspector_has_focus {
                                let ui = &mut self.ws.ui_root;
                                ui.inspector.clear_effect_selection(&mut ui.tree);
                            }
                            self.input_handler.inspector_has_focus = in_inspector;

                            // If a dropdown is open and the click lands outside it,
                            // dismiss the dropdown and consume the event so that the
                            // background node never receives a PointerDown (prevents
                            // phantom pressed_id on the node behind the dropdown).
                            if self.ws.ui_root.dropdown.is_open()
                                && !self.ws.ui_root.dropdown.contains_point(self.cursor_pos)
                            {
                                self.ws.ui_root.dropdown.close(&mut self.ws.ui_root.tree);
                                // Click is consumed by dismiss — do not forward.
                                if manifold_ui::input::input_trace_enabled() {
                                    eprintln!(
                                        "[input-trace] window: PRESS ({:.0},{:.0}) consumed by \
                                         dropdown-dismiss",
                                        self.cursor_pos.x, self.cursor_pos.y
                                    );
                                }
                            } else if self
                                .ws
                                .ui_root
                                .layout
                                .is_near_split_handle(self.cursor_pos)
                                && !self.ws.ui_root.overlay_contains_point(self.cursor_pos)
                            {
                                // D5 (`docs/DRAG_CAPTURE_DESIGN.md`): the seam
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
                                    self.needs_rebuild = true;
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
                                    self.needs_rebuild = true;
                                } else {
                                    self.audio_setup_handle_last_click = Some(std::time::Instant::now());
                                    self.ws.ui_root.begin_audio_setup_resize(self.cursor_pos.x);
                                    self.ws.ui_root.set_audio_setup_handle_drag();
                                }
                            } else if self.ws.ui_root.is_near_scene_setup_edge(self.cursor_pos)
                                && !self.ws.ui_root.overlay_contains_point(self.cursor_pos)
                            {
                                // Scene Setup dock resize handle — mirror of
                                // the Audio Setup one above (D2).
                                if self.is_double_click(self.scene_setup_handle_last_click) {
                                    self.scene_setup_handle_last_click = None;
                                    self.ws.ui_root.layout.reset_scene_setup_width();
                                    self.needs_rebuild = true;
                                } else {
                                    self.scene_setup_handle_last_click = Some(std::time::Instant::now());
                                    self.ws.ui_root.begin_scene_setup_resize(self.cursor_pos.x);
                                    self.ws.ui_root.set_scene_setup_handle_drag();
                                }
                            } else if self.ws.ui_root.is_near_inspector_edge(self.cursor_pos)
                                && !self.ws.ui_root.overlay_contains_point(self.cursor_pos)
                            {
                                // D5 — same seam-yields-to-overlay guard as
                                // the split handle above.
                                if self.is_double_click(self.inspector_handle_last_click) {
                                    self.inspector_handle_last_click = None;
                                    self.ws.ui_root.layout.reset_inspector_width();
                                    self.needs_rebuild = true;
                                } else {
                                    self.inspector_handle_last_click = Some(std::time::Instant::now());
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
                            } else {
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
                            if self.split_dragging {
                                // End video/timeline split drag.
                                // From Unity PanelResizeHandle.OnPointerUp.
                                self.split_dragging = false;
                                self.cursor_manager.set_default();
                                self.ws.ui_root.set_split_handle_idle();
                                // Width/ratio are captured at save time from
                                // the live UI layout (save_viewport_state) —
                                // a write here would be clobbered by the next
                                // content snapshot clone of local_project.
                            } else if self.ws.ui_root.inspector_resize_dragging {
                                self.ws.ui_root.end_inspector_resize();
                            } else if self.ws.ui_root.audio_setup_resize_dragging {
                                self.ws.ui_root.end_audio_setup_resize();
                                self.cursor_manager.set_default();
                                self.ws.ui_root.set_audio_setup_handle_idle();
                            } else if self.ws.ui_root.scene_setup_resize_dragging {
                                self.ws.ui_root.end_scene_setup_resize();
                                self.cursor_manager.set_default();
                                self.ws.ui_root.set_scene_setup_handle_idle();
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
                MouseButton::Right => {
                    if state == ElementState::Pressed {
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
                    } else {
                        // → Windowed: restore saved frame + menu bar
                        ws.window.set_decorations(true);
                        if let Some(frame) = self.output_saved_frame.take() {
                            ws.window.set_outer_position(winit::dpi::PhysicalPosition::new(
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
    /// dropdown scroll, then timeline zoom / pan / vertical scroll.
    pub(crate) fn primary_mouse_wheel(&mut self, is_primary: bool, delta: MouseScrollDelta) {
        if is_primary && self.perform_handle_mouse_wheel() {
            return;
        }
        if is_primary {
            // When the dropdown is open, route scroll to the UIEvent
            // pipeline so the dropdown can handle it.
            if self.ws.ui_root.dropdown.is_open() {
                let (dx, dy) = normalize_scroll_delta(delta);
                self.ws
                    .ui_root
                    .input
                    .process_scroll(self.cursor_pos, Vec2::new(dx, dy));
                return;
            }
            // Convert line deltas (mouse wheel notches) to logical pixels.
            // Each downstream consumer applies its own speed constant on top.
            let (dx, dy) = normalize_scroll_delta(delta);

            let pos = self.cursor_pos;
            let inspector_rect = self.ws.ui_root.layout.inspector();
            let tracks_rect = self.ws.ui_root.layout.timeline_tracks();

            // BUG-199/BUG-219: Audio Setup / Scene Setup docks — route
            // through the generic `UIEvent::Scroll` pipeline (same mechanism
            // the open-dropdown branch above uses) so the real app and the
            // headless `Gesture::Scroll` harness share one path. `contains()`
            // is already the open-check: a closed dock's rect is
            // `Rect::ZERO`. BUG-199's original fix assumed "the docks
            // rebuild every frame" — false: `app_render.rs`'s
            // `apply_ui_frame_invalidations` only rebuilds when
            // `needs_rebuild`/`scroll_dirty` says so (BUG-219 escaped
            // because the headless script harness always forces one rebuild
            // on its first dispatched gesture, via `Inspector::
            // skip_to_settled`'s `settled` flag — masking a scroll that
            // updates internal offset state but never gets baked into node
            // positions). Set `needs_rebuild` explicitly, same as the
            // inspector branch below.
            if self.ws.ui_root.layout.scene_setup().contains(pos)
                || self.ws.ui_root.layout.audio_setup().contains(pos)
            {
                self.ws
                    .ui_root
                    .input
                    .process_scroll(self.cursor_pos, Vec2::new(dx, dy));
                self.needs_rebuild = true;
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
                    self.needs_rebuild = true;
                }
            } else if tracks_rect.contains(pos) {
                if self.modifiers.alt {
                    // Alt + scroll Y → continuous, cursor-anchored zoom (§24 5e):
                    // smooth scaling of pixels-per-beat (not a jump between ten
                    // fixed levels), anchored on the beat under the mouse so the
                    // view zooms toward where you're pointing.
                    if dy.abs() > 0.01 {
                        let beat_at_cursor =
                            self.ws.ui_root.viewport.pixel_to_beat(pos.x).as_f32();
                        let exponent = dy / LINE_DELTA_PX;
                        let factor = manifold_ui::color::ZOOM_WHEEL_STEP_PER_NOTCH.powf(exponent);
                        let new_ppb = self.ws.ui_root.viewport.zoom_continuous(factor);
                        self.ws.ui_root.viewport.zoom_to(new_ppb, beat_at_cursor, pos.x);
                        self.scroll_dirty.zoom = true;
                    }
                } else if self.modifiers.shift {
                    // Shift + scroll Y → horizontal pan
                    let ppb = self.ws.ui_root.viewport.pixels_per_beat();
                    let beat_delta = dy * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                    let new_x =
                        (self.ws.ui_root.viewport.scroll_x_beats().as_f32() - beat_delta).max(0.0);
                    if self
                        .ws
                        .ui_root
                        .viewport
                        .set_scroll(new_x, self.ws.ui_root.viewport.scroll_y_px())
                    {
                        self.scroll_dirty.scroll_x = true;
                    }
                    // BUG-159: a direct user horizontal gesture — playhead-follow
                    // yields to it instead of fighting it (state_sync.rs
                    // check_auto_scroll).
                    self.ws.ui_root.viewport.note_user_scroll_x();
                } else {
                    // Plain scroll → vertical track scroll
                    let new_y = (self.ws.ui_root.viewport.scroll_y_px() - dy).max(0.0);
                    if self.ws.ui_root.viewport.set_scroll(
                        self.ws.ui_root.viewport.scroll_x_beats().as_f32(),
                        new_y,
                    ) {
                        // Layer headers read the viewport's scroll_y_px live at
                        // the next build/update — no separate push (D2).
                        self.scroll_dirty.scroll_y = true;
                    }
                }
                // Native horizontal scroll (trackpad two-finger swipe)
                if dx.abs() > 0.01 && !self.modifiers.alt {
                    let ppb = self.ws.ui_root.viewport.pixels_per_beat();
                    let beat_delta = dx * manifold_ui::color::SCROLL_SENSITIVITY / ppb;
                    let new_x =
                        (self.ws.ui_root.viewport.scroll_x_beats().as_f32() - beat_delta).max(0.0);
                    if self
                        .ws
                        .ui_root
                        .viewport
                        .set_scroll(new_x, self.ws.ui_root.viewport.scroll_y_px())
                    {
                        self.scroll_dirty.scroll_x = true;
                    }
                    // BUG-159: see the Shift+scroll site above.
                    self.ws.ui_root.viewport.note_user_scroll_x();
                }
            }
        }
    }

    /// `CursorMoved` in the editor window: update the canvas cursor + popovers,
    /// and forward into the editor UITree only when the cursor is in a margin
    /// (palette / sidebar) or the node picker is open.
    pub(crate) fn editor_cursor_moved(
        &mut self,
        window_id: WindowId,
        position: PhysicalPosition<f64>,
    ) {
        let (scale, window_w, window_h) = self
            .window_registry
            .get(&window_id)
            .map(|ws| {
                let s = ws.window.scale_factor();
                let sz = ws.window.inner_size();
                (s, sz.width as f32 / s as f32, sz.height as f32 / s as f32)
            })
            .unwrap_or((1.0, 1.0, 1.0));
        let logical_x = position.x as f32 / scale as f32;
        let logical_y = position.y as f32 / scale as f32;
        let area = manifold_ui::Rect::new(0.0, 0.0, window_w, window_h);
        let pos = Vec2::new(logical_x, logical_y);

        // Column-divider drag / hover takes precedence over the canvas +
        // panels. A live drag resizes the dock and consumes the move; otherwise
        // update the hover highlight (repaint only when it changes).
        let mut dock_dragging = false;
        let mut dock_cursor = TimelineCursor::Default;
        if let Some(ed) = self.graph_editor.as_mut() {
            if ed.dock.is_dragging() {
                ed.dock.drag(area, pos);
                ed.offscreen_dirty = true;
                dock_dragging = true;
            } else {
                let before = ed.dock.highlighted();
                ed.dock.set_hover_from(area, pos);
                if ed.dock.highlighted() != before {
                    ed.offscreen_dirty = true;
                }
            }
            dock_cursor = ed.dock.cursor().unwrap_or(TimelineCursor::Default);
        }
        self.cursor_manager.set(dock_cursor);
        self.apply_pending_cursor(window_id);
        if dock_dragging {
            return;
        }

        // Mini-timeline scrub: a move while scrubbing seeks the playhead and
        // consumes the move (the canvas + panels don't see it), mirroring the
        // dock-drag precedence above.
        if self.graph_editor.as_ref().is_some_and(|ed| ed.timeline_scrubbing) {
            let bottom = self
                .graph_editor
                .as_ref()
                .map(|ed| ed.dock.rects(area).bottom);
            if let Some(bottom) = bottom {
                let total = self.local_project.timeline.duration_beats().as_f32();
                let beat = manifold_ui::MiniTimeline::beat_at_x(bottom, total, logical_x);
                self.send_content_cmd(ContentCommand::SeekToBeat(
                    manifold_core::Beats::from_f32(beat),
                ));
                if let Some(ed) = self.graph_editor.as_mut() {
                    ed.offscreen_dirty = true;
                }
            }
            return;
        }

        // Preview sidebar on the left, card lane on the right (matches the main
        // timeline's inspector docking right). Geometry from the same `Dock` the
        // present pass reads, so the canvas viewport tracks the dragged columns.
        let (preview_width, card_x, viewport) = self
            .graph_editor
            .as_ref()
            .map(|ed| {
                let r = ed.dock.rects(area);
                (
                    r.left.width,
                    r.right.x,
                    // Canvas viewport matches the render-time slice (offset by
                    // preview_width); without this the canvas's `to_graph` would
                    // treat screen x=0 as the canvas left edge and node
                    // hit-tests would be off by `preview_width` to the left.
                    crate::graph_canvas::Rect::new(
                        r.canvas.x,
                        r.canvas.y,
                        r.canvas.width,
                        r.canvas.height,
                    ),
                )
            })
            .unwrap_or_else(|| {
                (0.0, window_w, crate::graph_canvas::Rect::new(0.0, 0.0, window_w, window_h))
            });
        // Always update canvas cursor — graph-space coords
        // need it even for clicks that land in the sidebar.
        if let Some(canvas) = self.graph_canvas.as_mut() {
            canvas.on_pointer_move(viewport, logical_x, logical_y);
            // Drive the mapping popover's live range drag /
            // handle hover. No-op when the popover is closed.
            canvas.popover_on_move(logical_x, logical_y);
        }
        // Same for the editor card's sideways mapping drawer (a
        // separate popover anchored on the right-lane card row).
        self.editor_mapping_popover.on_move(logical_x, logical_y);
        // Forward into the editor's UITree only when the
        // cursor sits in either margin (preview sidebar on the
        // left or the card lane on the right). Move events from
        // the canvas region would just cause spurious hover/exit
        // on tree nodes — except when the node picker is open,
        // which overlays the whole window and wants hover
        // feedback on its cells everywhere.
        let in_panel = logical_x < preview_width || logical_x >= card_x;
        let picker_open = self
            .graph_editor
            .as_ref()
            .is_some_and(|ed| ed.ui_root.browser_popup.is_open());
        if (in_panel || picker_open)
            && let Some(ed) = self.graph_editor.as_mut()
        {
            ed.ui_root.input.process_pointer(
                &mut ed.ui_root.tree,
                Vec2::new(logical_x, logical_y),
                manifold_ui::input::PointerAction::Move,
                self.time_since_start,
            );
        }
        if let Some(ed) = self.graph_editor.as_mut() {
            ed.offscreen_dirty = true;
        }
    }

    /// `MouseInput` in the editor window: node-picker-modal routing first, then
    /// the canvas / popovers / palette-vs-sidebar split.
    pub(crate) fn editor_mouse_input(
        &mut self,
        window_id: WindowId,
        button: MouseButton,
        state: ElementState,
    ) {
        // Node picker is modal: when open it claims every click in
        // the editor window. Route the press/release straight into
        // the editor UITree (which holds the popup's backdrop +
        // cells) and bypass the canvas / popover / in_panel split.
        // The resulting Click event is drained + dispatched to the
        // popup in `present_*` / the editor drain loop.
        let picker_open = self
            .graph_editor
            .as_ref()
            .is_some_and(|ed| ed.ui_root.browser_popup.is_open());
        if picker_open {
            // Cursor in editor-window logical coords. The canvas
            // tracks it on CursorMoved; read it out before the
            // mutable editor borrow.
            let (cx, cy) = self
                .graph_canvas
                .as_ref()
                .map(|c| c.cursor())
                .unwrap_or((0.0, 0.0));
            if button == MouseButton::Left
                && let Some(ed) = self.graph_editor.as_mut()
            {
                let action = match state {
                    ElementState::Pressed => {
                        manifold_ui::input::PointerAction::Down
                    }
                    ElementState::Released => {
                        manifold_ui::input::PointerAction::Up
                    }
                };
                ed.ui_root.input.process_pointer(
                    &mut ed.ui_root.tree,
                    Vec2::new(cx, cy),
                    action,
                    self.time_since_start,
                );
                ed.offscreen_dirty = true;
            }
            return;
        }
        // Column-divider drag takes precedence over the canvas + panels: a
        // press on a handle begins the drag, a release ends it. Handled on the
        // `graph_editor` workspace (a field disjoint from `graph_canvas`), so
        // the canvas never sees these clicks.
        {
            let (cx, cy) = self
                .graph_canvas
                .as_ref()
                .map(|c| c.cursor())
                .unwrap_or((0.0, 0.0));
            let (ww, wh) = self
                .window_registry
                .get(&window_id)
                .map(|ws| {
                    let s = ws.window.scale_factor();
                    let sz = ws.window.inner_size();
                    (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                })
                .unwrap_or((1.0, 1.0));
            let area = manifold_ui::Rect::new(0.0, 0.0, ww, wh);
            if button == MouseButton::Left
                && let Some(ed) = self.graph_editor.as_mut()
            {
                match state {
                    ElementState::Pressed => {
                        let press = Vec2::new(cx, cy);
                        if let Some(edge) = ed.dock.hit_test(area, press) {
                            ed.dock.begin(edge, press);
                            ed.offscreen_dirty = true;
                            return;
                        }
                    }
                    ElementState::Released => {
                        if ed.dock.is_dragging() {
                            ed.dock.end();
                            ed.offscreen_dirty = true;
                            return;
                        }
                    }
                }
            }
        }
        // Bottom mini-timeline: a press in the strip body starts a scrub (and
        // seeks immediately); a press on the play button toggles transport.
        // Handled on the disjoint `graph_editor` workspace, before the canvas
        // sees the click. The `bottom` rect is copied out so no borrow is held
        // across the content-command sends.
        if button == MouseButton::Left {
            let (cx, cy) = self
                .graph_canvas
                .as_ref()
                .map(|c| c.cursor())
                .unwrap_or((0.0, 0.0));
            let (ww, wh) = self
                .window_registry
                .get(&window_id)
                .map(|ws| {
                    let s = ws.window.scale_factor();
                    let sz = ws.window.inner_size();
                    (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                })
                .unwrap_or((1.0, 1.0));
            let area = manifold_ui::Rect::new(0.0, 0.0, ww, wh);
            let bottom = self
                .graph_editor
                .as_ref()
                .filter(|ed| ed.dock.show_bottom)
                .map(|ed| ed.dock.rects(area).bottom);
            if let Some(bottom) = bottom {
                let pos = Vec2::new(cx, cy);
                match state {
                    ElementState::Pressed => {
                        if manifold_ui::MiniTimeline::hit_play(bottom, pos) {
                            let cmd = if self.content_state.is_playing {
                                ContentCommand::Pause
                            } else {
                                ContentCommand::Play
                            };
                            self.send_content_cmd(cmd);
                            if let Some(ed) = self.graph_editor.as_mut() {
                                ed.offscreen_dirty = true;
                            }
                            return;
                        }
                        // Gutter click: pick that row's layer (identify + choose,
                        // same effect a main-timeline layer-header click has —
                        // `active_layer_id` + `selection.select_layer`), so the
                        // editor's mirrored inspector follows it too. Checked
                        // before the scrub body since the gutter sits outside it.
                        if let Some(row) = manifold_ui::MiniTimeline::row_at_y(
                            bottom,
                            self.local_project.timeline.layers.len(),
                            pos,
                        ) && let Some(layer) = self.local_project.timeline.layers.get(row)
                        {
                            let layer_id = layer.layer_id.clone();
                            self.active_layer_id = Some(layer_id.clone());
                            self.selection.select_layer(layer_id);
                            self.needs_rebuild = true;
                            if let Some(ed) = self.graph_editor.as_mut() {
                                ed.offscreen_dirty = true;
                            }
                            return;
                        }
                        if manifold_ui::MiniTimeline::body_rect(bottom).contains(pos) {
                            let total =
                                self.local_project.timeline.duration_beats().as_f32();
                            let beat =
                                manifold_ui::MiniTimeline::beat_at_x(bottom, total, cx);
                            self.send_content_cmd(ContentCommand::SeekToBeat(
                                manifold_core::Beats::from_f32(beat),
                            ));
                            if let Some(ed) = self.graph_editor.as_mut() {
                                ed.timeline_scrubbing = true;
                                ed.offscreen_dirty = true;
                            }
                            return;
                        }
                    }
                    ElementState::Released => {
                        if let Some(ed) = self.graph_editor.as_mut()
                            && std::mem::take(&mut ed.timeline_scrubbing)
                        {
                            ed.offscreen_dirty = true;
                            return;
                        }
                    }
                }
            }
        }
        if let Some(canvas) = self.graph_canvas.as_mut() {
            let window_size = self
                .window_registry
                .get(&window_id)
                .map(|ws| {
                    let s = ws.window.scale_factor();
                    let sz = ws.window.inner_size();
                    (sz.width as f32 / s as f32, sz.height as f32 / s as f32)
                })
                .unwrap_or((1.0, 1.0));
            let (cx, cy) = canvas.cursor();
            // Geometry from the same `Dock` the present pass reads, so column
            // resizes move the canvas viewport + panel split in lockstep.
            let area = manifold_ui::Rect::new(0.0, 0.0, window_size.0, window_size.1);
            let (preview_width, card_x, viewport) = self
                .graph_editor
                .as_ref()
                .map(|ed| {
                    let r = ed.dock.rects(area);
                    (
                        r.left.width,
                        r.right.x,
                        // Canvas viewport matches the render-time slice: origin
                        // at the left column, width is the center column. Passing
                        // this (not the full window) is what makes `to_graph`
                        // translate cursor coords correctly.
                        crate::graph_canvas::Rect::new(
                            r.canvas.x,
                            r.canvas.y,
                            r.canvas.width,
                            r.canvas.height,
                        ),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        0.0,
                        window_size.0,
                        crate::graph_canvas::Rect::new(0.0, 0.0, window_size.0, window_size.1),
                    )
                });
            // The UITree spans the whole editor window — both the left preview
            // sidebar and the right card lane live in it. Route any click in
            // either margin to it; the canvas only sees clicks in the center.
            let in_panel = cx < preview_width || cx >= card_x;
            match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    // The editor card's mapping drawer floats over
                    // the canvas (anchored beside the right-lane
                    // card), so it gets first crack. Consumed → done;
                    // open-but-missed → close it and fall through.
                    let mut consumed = false;
                    if self.editor_mapping_popover.is_open() {
                        if self.editor_mapping_popover.on_press(cx, cy) {
                            consumed = true;
                        } else {
                            self.editor_mapping_popover.close();
                        }
                    }
                    // The canvas popover (on-node rows) floats over
                    // the canvas too — next crack. If neither popover
                    // takes it, fall through to the panel/canvas path.
                    if consumed {
                        // handled by the editor mapping drawer
                    } else if canvas.popover_open()
                        && canvas.popover_on_left_press(cx, cy)
                    {
                        // consumed by the canvas popover
                    } else if in_panel {
                        // Right lane = the inspector column (left = preview
                        // monitors): forward the press into its UITree. A card
                        // click retargets the canvas via the
                        // EffectCardClicked/GenCardClicked dispatch pass; sliders,
                        // tabs, chrome and drags all resolve through the inspector.
                        if let Some(ed) = self.graph_editor.as_mut() {
                            ed.ui_root.input.process_pointer(
                                &mut ed.ui_root.tree,
                                Vec2::new(cx, cy),
                                manifold_ui::input::PointerAction::Down,
                                self.time_since_start,
                            );
                        }
                    } else {
                        canvas.on_left_button_down(
                            viewport,
                            cx,
                            cy,
                            self.time_since_start,
                            self.modifiers.shift,
                        );
                    }
                }
                (MouseButton::Left, ElementState::Released) => {
                    // Commit any in-progress drawer/popover drag.
                    self.editor_mapping_popover.on_release();
                    canvas.popover_on_left_release();
                    if in_panel {
                        if let Some(ed) = self.graph_editor.as_mut() {
                            ed.ui_root.input.process_pointer(
                                &mut ed.ui_root.tree,
                                Vec2::new(cx, cy),
                                manifold_ui::input::PointerAction::Up,
                                self.time_since_start,
                            );
                        }
                    } else {
                        canvas.on_left_button_up(viewport, cx, cy);
                    }
                }
                (MouseButton::Right, ElementState::Pressed) => {
                    // Right-click an expanded param row that's
                    // exposed as a card binding → open the
                    // in-place mapping popover anchored on it.
                    // The canvas resolves the row + anchor; the
                    // app resolves the binding (needs the
                    // project + snapshot the canvas can't see).
                    if !in_panel
                        && let Some((node_id, pi)) =
                            canvas.on_right_button_down(viewport, cx, cy)
                        && let Some(inner_param) = canvas.param_name_at(node_id, pi)
                        && let Some((
                            binding_id,
                            label,
                            min,
                            max,
                            invert,
                            curve,
                            scale,
                            offset,
                            range,
                            section,
                        )) = crate::app_render::resolve_canvas_binding(
                            self.content_state.active_graph_snapshot.as_deref(),
                            self.watched_graph_target.as_ref(),
                            &self.local_project,
                            node_id,
                            &inner_param,
                        )
                    {
                        canvas.open_mapping_popover(
                            viewport, node_id, pi, binding_id, label, min, max, invert,
                            crate::ui_translate::macro_curve_to_ui(curve), scale, offset, range,
                            section,
                        );
                    }
                }
                (MouseButton::Middle, ElementState::Pressed) => {
                    if !in_panel {
                        canvas.on_pan_button_down(cx, cy);
                    }
                }
                (MouseButton::Middle, ElementState::Released) => {
                    if !in_panel {
                        canvas.on_pan_button_up();
                    }
                }
                _ => {}
            }
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
        }
    }

    /// `MouseWheel` in the editor window: cursor-anchored canvas zoom. Returns
    /// `true` when handled (a canvas is open); `false` falls through to the
    /// primary path, matching the old `is_graph_editor && let Some(canvas)` gate.
    pub(crate) fn editor_mouse_wheel(
        &mut self,
        window_id: WindowId,
        delta: MouseScrollDelta,
    ) -> bool {
        // Right lane = the inspector column: a wheel over it scrolls the inspector
        // (same `handle_scroll_at` convention as the main window), not the canvas.
        // The editor rebuilds its tree every present, so updating the scroll offset
        // here takes effect on the next build_inspector_in_rect. Cursor is tracked
        // by the canvas; column geometry from the same `Dock` the present reads.
        let (cx, _cy) = self
            .graph_canvas
            .as_ref()
            .map(|c| c.cursor())
            .unwrap_or((0.0, 0.0));
        let card_x = self.graph_editor.as_ref().map(|ed| {
            let (s, sz) = {
                let w = &self.window_registry.get(&window_id);
                w.map(|ws| (ws.window.scale_factor(), ws.window.inner_size()))
                    .unwrap_or((1.0, winit::dpi::PhysicalSize::new(1, 1)))
            };
            let area = manifold_ui::Rect::new(
                0.0,
                0.0,
                sz.width as f32 / s as f32,
                sz.height as f32 / s as f32,
            );
            ed.dock.rects(area).right.x
        });
        if let Some(card_x) = card_x
            && cx >= card_x
        {
            let (_dx, dy) = normalize_scroll_delta(delta);
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.ui_root.inspector.handle_scroll_at(dy, cx);
                ed.offscreen_dirty = true;
            }
            return true;
        }
        if let Some(canvas) = self.graph_canvas.as_mut() {
            const LINE_DELTA_PX: f32 = 20.0;
            let dy = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => y * LINE_DELTA_PX,
                winit::event::MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
            };
            let viewport = self
                .window_registry
                .get(&window_id)
                .map(|ws| {
                    let s = ws.window.scale_factor();
                    let sz = ws.window.inner_size();
                    crate::graph_canvas::Rect::new(
                        0.0,
                        0.0,
                        sz.width as f32 / s as f32,
                        sz.height as f32 / s as f32,
                    )
                })
                .unwrap_or(crate::graph_canvas::Rect::new(0.0, 0.0, 1.0, 1.0));
            canvas.on_scroll(viewport, dy);
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
            true
        } else {
            false
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
        // Mapping drawer numeric entry: when a value field is active,
        // keystrokes type into it (digits / `.` / `-` / Backspace),
        // Enter commits, Esc cancels — ahead of any canvas shortcut
        // (Backspace would otherwise delete the selected node). Plain
        // chars only (no Cmd/Ctrl) so shortcuts still pass when not
        // typing a value.
        {
            use winit::keyboard::{Key, NamedKey};
            let typing = !self.modifiers.command && !self.modifiers.ctrl;
            if self.editor_mapping_popover.is_editing() {
                match &logical_key {
                    Key::Named(NamedKey::Enter) => self.editor_mapping_popover.commit_edit(),
                    Key::Named(NamedKey::Escape) => self.editor_mapping_popover.cancel_edit(),
                    Key::Named(NamedKey::Backspace) => {
                        self.editor_mapping_popover.on_backspace()
                    }
                    Key::Named(NamedKey::Space) if typing => {
                        self.editor_mapping_popover.on_text_char(' ')
                    }
                    Key::Character(c) if typing => {
                        for ch in c.chars() {
                            self.editor_mapping_popover.on_text_char(ch);
                        }
                    }
                    _ => {}
                }
                if let Some(ed) = self.graph_editor.as_mut() {
                    ed.offscreen_dirty = true;
                }
                return true;
            }
            if let Some(canvas) = self.graph_canvas.as_mut()
                && canvas.popover_is_editing()
            {
                match &logical_key {
                    Key::Named(NamedKey::Enter) => canvas.popover_commit_edit(),
                    Key::Named(NamedKey::Escape) => canvas.popover_cancel_edit(),
                    Key::Named(NamedKey::Backspace) => canvas.popover_on_backspace(),
                    Key::Named(NamedKey::Space) if typing => {
                        canvas.popover_on_text_char(' ')
                    }
                    Key::Character(c) if typing => {
                        for ch in c.chars() {
                            canvas.popover_on_text_char(ch);
                        }
                    }
                    _ => {}
                }
                if let Some(ed) = self.graph_editor.as_mut() {
                    ed.offscreen_dirty = true;
                }
                return true;
            }
        }
        // Editor window: node picker is open → keystrokes drive its
        // search field. Must run BEFORE the Delete/Backspace node-
        // delete arm below, or Backspace would delete a node instead
        // of editing the filter. Escape dismisses; chars/space type;
        // every transition sets offscreen_dirty so the editor
        // repaints (it has no per-frame repaint loop when idle).
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
                // Picker keyboard nav (P2): arrows move the grid cursor.
                Key::Named(NamedKey::ArrowUp) => {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root
                            .browser_popup
                            .handle_key_nav(manifold_ui::input::Key::Up);
                    }
                }
                Key::Named(NamedKey::ArrowDown) => {
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root
                            .browser_popup
                            .handle_key_nav(manifold_ui::input::Key::Down);
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
                    ed.ui_root.browser_popup.set_filter(filter);
                }
            }
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
            return true;
        }
        // Editor window: a graph text field (group rename / String param /
        // wgsl / node search) is active → keystrokes type into it. Ahead
        // of node-delete + navigation so Backspace edits text and Esc
        // cancels the edit rather than deleting a node or leaving a group.
        if is_graph_editor
            && self.text_input.active
            && self.text_input.field.is_graph_field()
        {
            use winit::keyboard::{Key, NamedKey};
            let typing = !self.modifiers.command && !self.modifiers.ctrl;
            let was_search = self.text_input.field
                == crate::text_input::TextInputField::GraphNodeSearch;
            match &logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.text_input.cancel();
                    if was_search && let Some(canvas) = self.graph_canvas.as_mut() {
                        canvas.set_node_search("");
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    // Multiline (WGSL code): Enter inserts a newline, the
                    // natural code-editor convention; Cmd+Enter commits.
                    // Single-line fields commit on a bare Enter.
                    if self.text_input.multiline && !self.modifiers.command {
                        self.text_input.insert_char('\n');
                    } else {
                        let (field, text) = self.text_input.commit();
                        self.handle_text_input_commit(field, &text);
                    }
                }
                Key::Named(NamedKey::Backspace) => self.text_input.backspace(),
                Key::Named(NamedKey::Delete) => self.text_input.delete(),
                Key::Named(NamedKey::ArrowLeft) => {
                    if self.modifiers.command {
                        self.text_input.move_home(self.modifiers.shift);
                    } else {
                        self.text_input.move_left(self.modifiers.shift, self.modifiers.alt);
                    }
                }
                Key::Named(NamedKey::ArrowRight) => {
                    if self.modifiers.command {
                        self.text_input.move_end(self.modifiers.shift);
                    } else {
                        self.text_input.move_right(self.modifiers.shift, self.modifiers.alt);
                    }
                }
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
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
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
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
            return true;
        }
        // Editor window: graph-canvas navigation + debug + transport keys.
        //   Space → toggle play/pause (preview motion while authoring), same
        //           as the mini-timeline play button. Reached only when no text
        //           field / popover is active (those arms returned above).
        //   Esc → leave the current group (pop one scope level), if
        //         the view is inside a group.
        //   `   → toggle the debug overlay HUD.
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
                Key::Character(c) if c.as_str() == "`" => {
                    if let Some(canvas) = self.graph_canvas.as_mut() {
                        canvas.toggle_debug_overlay();
                    }
                    handled = true;
                }
                _ => {}
            }
            if handled {
                if let Some(ed) = self.graph_editor.as_mut() {
                    ed.offscreen_dirty = true;
                }
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
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
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
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
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
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
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
                if let Some(ed) = self.graph_editor.as_mut() {
                    ed.offscreen_dirty = true;
                }
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
                .map(|vp| {
                    crate::text_input::AnchorRect::new(vp.x + 12.0, 12.0, 220.0, 18.0)
                })
                .unwrap_or_else(|| {
                    crate::text_input::AnchorRect::new(120.0, 12.0, 220.0, 18.0)
                });
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
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
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
                self.send_content_cmd(ContentCommand::Execute(Box::new(cmd)));
            }
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
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
            if let Some(tx) = self.content_tx.as_ref() {
                if self.modifiers.shift {
                    crate::ui_bridge::redo(tx);
                } else {
                    crate::ui_bridge::undo(tx);
                }
            }
            if let Some(ed) = self.graph_editor.as_mut() {
                ed.offscreen_dirty = true;
            }
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
        self.resize_graph_editor_offscreen(size.width.max(1), size.height.max(1));
        if let Some(ed) = self.graph_editor.as_mut() {
            ed.surface_resized_this_frame = true;
            ed.offscreen_dirty = true;
        }
    }

    /// `ScaleFactorChanged` in the editor window: re-resize from the new
    /// physical inner size (scale_factor itself is unused — the inner size
    /// already reflects it).
    pub(crate) fn editor_scale_factor_changed(
        &mut self,
        window_id: WindowId,
        scale_factor: f64,
    ) {
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
            ed.offscreen_dirty = true;
        }
        let _ = scale_factor;
    }

    /// Convert a `BrowserPopupAction::Selected` from the MAIN window's
    /// browser popup (Effect/Generator only — Node mode is editor-only and
    /// never reaches this path) into the equivalent `PanelAction` and stash
    /// it on `pending_keyboard_actions` for `process_events` to dispatch next
    /// frame. Mirrors the click-arm translation in `browser_popup.rs`'s
    /// `Overlay::on_event` impl, just reached from a keyboard pick
    /// (`OVERLAY_SESSIONS_AND_PICKER_DESIGN.md` §5 P2 arrow/Enter nav)
    /// instead of a mouse click.
    fn stash_browser_popup_pick(
        &mut self,
        action: manifold_ui::panels::browser_popup::BrowserPopupAction,
    ) {
        use manifold_ui::panels::PanelAction;
        use manifold_ui::panels::browser_popup::{BrowserPopupAction, BrowserPopupMode};
        if let BrowserPopupAction::Selected {
            type_id,
            mode,
            tab,
            layer_id,
        } = action
        {
            let panel_action = match mode {
                BrowserPopupMode::Effect => {
                    PanelAction::AddEffect(tab, manifold_ui::types::PresetTypeId::from_string(type_id))
                }
                BrowserPopupMode::Generator => PanelAction::SetGenType(
                    layer_id,
                    manifold_ui::types::PresetTypeId::from_string(type_id),
                ),
                BrowserPopupMode::Node => return,
            };
            self.ws.ui_root.pending_keyboard_actions.push(panel_action);
        }
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
        let data_version_before = self.content_state.data_version;
        if is_primary {
            // Escape cancels an in-flight timeline clip move/trim before any
            // other Escape handling — a live drag holds uncommitted model
            // mutations (`InteractionOverlay::cancel_drag`, P1.4 D5/B8:
            // "restore and close batch", never commit-then-undo) that must
            // be rolled back before anything else touches the project.
            if matches!(logical_key, Key::Named(NamedKey::Escape))
                && matches!(
                    self.overlay.drag_mode(),
                    DragMode::Move | DragMode::TrimLeft | DragMode::TrimRight
                )
                && let Some(content_tx) = self.content_tx.as_ref()
            {
                let mut host = crate::editing_host::AppEditingHost::new(
                    &mut self.local_project,
                    content_tx,
                    &self.content_state,
                    &mut self.cursor_manager,
                    &mut self.active_layer_id,
                    &mut self.needs_rebuild,
                    &mut self.needs_structural_sync,
                    &mut self.scroll_dirty,
                    &mut self.invalidate_layers,
                    &mut self.pre_drag_commands,
                );
                self.overlay.cancel_drag(&mut host);
                return;
            }

            // Text input mode: intercept all keys for text editing
            if self.text_input.active {
                let is_search_filter =
                    self.text_input.field == crate::text_input::TextInputField::SearchFilter;
                match &logical_key {
                    Key::Named(NamedKey::Escape) => {
                        self.text_input.cancel();
                        // BUG-022: cancelling the search field alone left the
                        // browser popup open (a second Escape was needed).
                        // Close it here too, mirroring the editor window's
                        // node-picker Escape branch, so one press dismisses
                        // both. The closed-overlay pump then reconciles the
                        // (already-cancelled) text session next frame.
                        if is_search_filter {
                            self.ws.ui_root.browser_popup.handle_escape();
                        }
                        consumed = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if self.text_input.multiline && self.modifiers.shift {
                            self.text_input.insert_char('\n');
                        } else if is_search_filter {
                            // Type-and-enter fast path (P2): pick straight
                            // from the picker instead of just committing
                            // filter text (the reactive-search block below
                            // already keeps that live on every keystroke).
                            // If the popup closed as a result (a pick, or an
                            // empty-filtered-list Escape-equivalent), the
                            // closed-overlay pump cleans up this text
                            // session next `process_events` — no manual
                            // cancel needed here.
                            if let Some(action) =
                                self.ws.ui_root.browser_popup.handle_key_nav(manifold_ui::input::Key::Enter)
                            {
                                self.stash_browser_popup_pick(action);
                            }
                        } else {
                            let (field, text) = self.text_input.commit();
                            self.handle_text_input_commit(field, &text);
                        }
                        consumed = true;
                    }
                    Key::Named(NamedKey::Backspace) => {
                        self.text_input.backspace();
                        consumed = true;
                    }
                    Key::Named(NamedKey::Delete) => {
                        self.text_input.delete();
                        consumed = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        if self.modifiers.command {
                            self.text_input.move_home(self.modifiers.shift);
                        } else {
                            self.text_input.move_left(self.modifiers.shift, self.modifiers.alt);
                        }
                        consumed = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        if self.modifiers.command {
                            self.text_input.move_end(self.modifiers.shift);
                        } else {
                            self.text_input.move_right(self.modifiers.shift, self.modifiers.alt);
                        }
                        consumed = true;
                    }
                    // Picker keyboard nav (P2) — only meaningful while the
                    // browser search field owns the session; every other
                    // active text field suppresses arrows (falls to `_`,
                    // unchanged from before).
                    Key::Named(NamedKey::ArrowUp) if is_search_filter => {
                        self.ws.ui_root.browser_popup.handle_key_nav(manifold_ui::input::Key::Up);
                        consumed = true;
                    }
                    Key::Named(NamedKey::ArrowDown) if is_search_filter => {
                        self.ws.ui_root.browser_popup.handle_key_nav(manifold_ui::input::Key::Down);
                        consumed = true;
                    }
                    Key::Named(NamedKey::Space) => {
                        self.text_input.insert_char(' ');
                        consumed = true;
                    }
                    Key::Character(c) => {
                        // Cmd+A / Ctrl+A → select all
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
                        } else {
                            for ch in c.chars() {
                                self.text_input.insert_char(ch);
                            }
                        }
                        consumed = true;
                    }
                    _ => {
                        consumed = true;
                    } // Suppress all other keys
                }
                // Reactive search: push filter on every keystroke
                if consumed
                    && self.text_input.field == crate::text_input::TextInputField::SearchFilter
                {
                    self.ws
                        .ui_root
                        .browser_popup
                        .set_filter(self.text_input.text().trim().to_string());
                    self.needs_rebuild = true;
                }
                // Skip normal shortcut processing when text input consumed the key
                if consumed {
                    return;
                }
            }
            // ── Shortcut dispatch via InputHandler ──
            // Port of Unity InputHandler.HandleKeyboardInput().
            // All viewport access goes through the TimelineInputHost trait.
            if !consumed && let Some(content_tx) = self.content_tx.as_ref() {
                let mut host = crate::input_host::AppInputHost {
                    project: &mut self.local_project,
                    content_tx,
                    content_state: &self.content_state,
                    ui_root: &mut self.ws.ui_root,
                    selection: &mut self.selection,
                    active_layer: &mut self.active_layer_id,
                    needs_rebuild: &mut self.needs_rebuild,
                    needs_structural_sync: &mut self.needs_structural_sync,
                    scroll_dirty: &mut self.scroll_dirty,
                    current_project_path: &self.current_project_path,
                    has_output_window: self.window_registry.has_output_window(),
                    pending_close_output: &mut self.pending_close_output,
                    pending_export: &mut self.pending_export,
                    effect_clipboard: &mut self.effect_clipboard,
                    project_io: &mut self.project_io,
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
                        self.local_project = project.clone();
                        self.suppress_snapshot_until = self.content_state.data_version + 1;
                        self.suppress_snapshot_set_at = self.frame_count;
                        self.send_content_cmd(ContentCommand::LoadProject(Box::new(project)));
                        self.send_content_cmd(ContentCommand::SetProject);
                        self.selection.clear_selection();
                        self.active_layer_id = self
                            .local_project
                            .timeline
                            .layers
                            .first()
                            .map(|l| l.layer_id.clone());
                        self.needs_rebuild = true;
                        log::info!("New project created");
                        consumed = true;
                    }

                    _ => {}
                }
            } // end if !consumed (file operations)
        } // end if is_primary

        // All other shortcuts handled by InputHandler → AppInputHost.

        // If any keyboard shortcut mutated project data, trigger structural sync
        if self.content_state.data_version != data_version_before {
            self.needs_structural_sync = true;
            self.needs_rebuild = true;
        }

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
        let (dx, dy) =
            normalize_scroll_delta(MouseScrollDelta::PixelDelta(PhysicalPosition::new(7.5, -4.25)));
        assert_eq!(dx, 7.5);
        assert_eq!(dy, -4.25);
    }
}
