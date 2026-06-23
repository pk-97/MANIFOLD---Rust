//! Graph-editor window input dispatch (Phase 4.6 — "fold the editor event loop
//! into the shared path").
//!
//! The editor window's pointer / scroll / keyboard handling used to be inlined
//! across ~20 `if is_graph_editor { … return; }` branches inside the single
//! `App::window_event` match — a second dispatch *policy* that drifted from the
//! primary timeline path (the audit's "two parallel event loops"). Those branch
//! bodies live here now as one named owner: `window_event` delegates to a single
//! method per arm, so the editor's input runs through one place instead of being
//! smeared across the match.
//!
//! Behaviour-preserving: each method is the verbatim old branch body. Event
//! values arrive by value (they're `Copy`, or `Key` cheap-clones once per
//! keypress) so the bodies move unchanged. The viewport-slice math
//! (`palette_width`/`sidebar_x`) and the explicit `offscreen_dirty` marking (the
//! editor has no idle repaint loop) are preserved exactly — losing either would
//! mis-hit-test or freeze the editor.

use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta};
use winit::keyboard::Key;
use winit::window::WindowId;

use manifold_ui::node::Vec2;

use crate::app::Application;
use crate::content_command::ContentCommand;

impl Application {
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
        let palette_width = manifold_ui::panels::graph_editor::EDITOR_CARD_LANE_WIDTH;
        let sidebar_x = window_w - manifold_ui::panels::graph_editor::SIDEBAR_WIDTH;
        // Canvas viewport matches the render-time slice
        // (offset by palette_width); without this the
        // canvas's `to_graph` would treat screen x=0 as the
        // canvas left edge and node hit-tests would be off
        // by `palette_width` to the left.
        let viewport = crate::graph_canvas::Rect::new(
            palette_width,
            0.0,
            (sidebar_x - palette_width).max(0.0),
            window_h,
        );
        // Always update canvas cursor — graph-space coords
        // need it even for clicks that land in the sidebar.
        if let Some(canvas) = self.graph_canvas.as_mut() {
            canvas.on_pointer_move(viewport, logical_x, logical_y);
            // Drive the mapping popover's live range drag /
            // handle hover. No-op when the popover is closed.
            canvas.popover_on_move(logical_x, logical_y);
        }
        // Same for the editor card's sideways mapping drawer (a
        // separate popover anchored on the left-lane card row).
        self.editor_mapping_popover.on_move(logical_x, logical_y);
        // Forward into the editor's UITree only when the
        // cursor sits in either margin (palette on the left
        // or expose-panel sidebar on the right). Move
        // events from the canvas region would just cause
        // spurious hover/exit on tree nodes — except when the
        // node picker is open, which overlays the whole window
        // and wants hover feedback on its cells everywhere.
        let in_panel = logical_x < palette_width || logical_x >= sidebar_x;
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
            let palette_width =
                manifold_ui::panels::graph_editor::EDITOR_CARD_LANE_WIDTH;
            let sidebar_x =
                window_size.0 - manifold_ui::panels::graph_editor::SIDEBAR_WIDTH;
            // The UITree spans the whole editor window — both
            // the left palette and the right sidebar live in
            // it. Route any click in either margin to it; the
            // canvas only sees clicks in the center column.
            let in_panel = cx < palette_width || cx >= sidebar_x;
            // Canvas viewport matches the render-time slice:
            // origin at palette_width, width is the remaining
            // center column. Passing this (not the full window)
            // is what makes `to_graph` translate cursor coords
            // into the canvas's coordinate system correctly.
            let viewport = crate::graph_canvas::Rect::new(
                palette_width,
                0.0,
                (sidebar_x - palette_width).max(0.0),
                window_size.1,
            );
            match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    // The editor card's mapping drawer floats over
                    // the canvas (anchored right of the left-lane
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
                        // Jump-to-node: a click on a card param's NAME
                        // in the left lane navigates the centre canvas
                        // to the node that param is exposed from, so
                        // the instrument and the graph stay in lockstep.
                        // Read-only on the card; only the canvas moves.
                        let mut jumped = false;
                        if cx < palette_width
                            && let Some(ed) = self.graph_editor.as_ref()
                            && let Some(param_id) =
                                self.editor_card.label_hit(&ed.ui_root.tree, cx, cy)
                            && let Some(snap) =
                                self.content_state.active_graph_snapshot.as_deref()
                            && let Some(node_id) =
                                crate::graph_canvas::resolve_card_param_node_id(
                                    snap, &param_id,
                                )
                        {
                            jumped = canvas.focus_node(snap, &node_id);
                        }
                        if !jumped
                            && let Some(ed) = self.graph_editor.as_mut()
                        {
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
                        )) = crate::app_render::resolve_canvas_binding(
                            self.content_state.active_graph_snapshot.as_deref(),
                            self.watched_graph_target.as_ref(),
                            &self.local_project,
                            node_id,
                            &inner_param,
                        )
                    {
                        canvas.open_mapping_popover(
                            viewport, node_id, pi, binding_id, label, min, max,
                            invert, curve, scale, offset, range,
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
                    if let Some(ed) = self.graph_editor.as_mut() {
                        ed.ui_root.browser_popup.handle_escape();
                    }
                    self.text_input.cancel();
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
                // Suppress every other key while the modal is up.
                _ => {}
            }
            if filter_changed {
                let filter = self.text_input.text.trim().to_string();
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
                Key::Named(NamedKey::ArrowLeft) => self.text_input.move_left(),
                Key::Named(NamedKey::ArrowRight) => self.text_input.move_right(),
                Key::Named(NamedKey::Space) if typing => self.text_input.insert_char(' '),
                Key::Character(c) => {
                    if c == "a" && self.modifiers.command {
                        self.text_input.select_all_text();
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
                let q = self.text_input.text.trim().to_string();
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
        // Editor window: graph-canvas navigation + debug keys.
        //   Esc → leave the current group (pop one scope level), if
        //         the view is inside a group.
        //   `   → toggle the debug overlay HUD.
        // (Ctrl+G group / Ctrl+Shift+G ungroup are handled below.)
        if is_graph_editor {
            use winit::keyboard::{Key, NamedKey};
            let mut handled = false;
            match &logical_key {
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
}
