//! Canvas interaction: the `DragMode` state machine, the per-event input
//! handlers the editor window calls (pointer/pan/left-button), selection and
//! scope navigation, the mapping-popover forwarders, and the `request_*`
//! command emitters. Every mutation is queued onto `pending_actions` and
//! drained by the editor window each event.

use super::*;

#[derive(Debug, Clone)]
pub(crate) enum DragMode {
    None,
    Pan,
    /// Dragging from an output port to draw a wire. On release over an
    /// input port, emits `GraphEditCommand::ConnectPorts`.
    WireFrom {
        from_node: u32,
        from_port: String,
    },
    /// Dragging a node by its header. `anchor_offset` is the graph-space
    /// (cursor - node_origin) at button-down so the node doesn't snap
    /// to the cursor on pickup. `start_pos` is the node's pre-drag
    /// position, retained so the `MoveGraphNode` action emitted on
    /// release reflects only the net delta and the undo command has a
    /// clean previous-pos to restore.
    NodeMove {
        node_id: u32,
        anchor_offset: (f32, f32),
        #[allow(dead_code)]
        start_pos: (f32, f32),
    },
    /// Scrubbing a numeric param on a node's face. Cumulative pixel delta
    /// from `press_origin_x` maps to a value delta over
    /// `PARAM_SCRUB_FULL_RANGE_PX`, anchored on `start_value` so a long
    /// drag doesn't accumulate float error. Emits `SetGraphNodeParam` each
    /// pointer move, matching the inspector sidebar.
    ParamScrub {
        node_id: u32,
        param_name: String,
        range: (f32, f32),
        start_value: f32,
        is_int: bool,
        press_origin_x: f32,
    },
    /// Rubber-band selection from a Shift+empty-canvas press. `origin_screen`
    /// is the press point; the live rect spans it to the current cursor. On
    /// release, the nodes the box intersects become the selection (replace).
    Marquee { origin_screen: (f32, f32) },
}

impl DragMode {
    fn is_pan(&self) -> bool {
        matches!(self, DragMode::Pan)
    }

    /// Short tag for the debug overlay readout.
    pub(crate) fn debug_label(&self) -> &'static str {
        match self {
            DragMode::None => "none",
            DragMode::Pan => "pan",
            DragMode::WireFrom { .. } => "wire",
            DragMode::NodeMove { .. } => "node-move",
            DragMode::ParamScrub { .. } => "param-scrub",
            DragMode::Marquee { .. } => "marquee",
        }
    }
}

impl GraphCanvas {
    /// Drain the queued graph edits accumulated from canvas interactions
    /// (wire connects, node moves/deletes, param scrubs, group ops). The app
    /// translates each into a `commands::graph::*`. Called once per input event
    /// by the editor window's present path.
    pub fn drain_edits(&mut self) -> Vec<GraphEditCommand> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Drain the canvas's mapping-popover edits (`EffectMapping*`), a separate
    /// `PanelAction` command family dispatched through the normal action path.
    pub fn drain_popover_actions(&mut self) -> Vec<PanelAction> {
        self.mapping_popover.drain_actions()
    }

    /// Emit a `RemoveGraphNode` action for every currently-selected node.
    /// Wired to the Delete/Backspace key handler on the editor window. Clears
    /// the selection on emit so the next frame doesn't double-fire. Multiple
    /// selected nodes each emit one action (and one undo entry apiece).
    pub fn request_delete_selected(&mut self) {
        for id in std::mem::take(&mut self.selected) {
            self.pending_actions
                .push(GraphEditCommand::RemoveGraphNode { node_id: id });
        }
    }

    /// Emit a `GroupSelection` action collapsing the current selection into a
    /// new group at this scope level. Wired to Ctrl+G. No-op on an empty
    /// selection. The new group's handle is auto-named (`group_N`) and made
    /// unique among the level's existing handles so flatten-time prefixing
    /// can't collide. The content thread validates the rest (boundary nodes,
    /// connectivity); a rejected group simply doesn't change the def.
    pub fn request_group_selection(&mut self) {
        let node_ids = self.selected_ids();
        if node_ids.is_empty() {
            return;
        }
        let existing: ahash::AHashSet<&str> =
            self.nodes.iter().filter_map(|n| n.handle.as_deref()).collect();
        let mut i = 1u32;
        let mut handle = format!("group_{i}");
        while existing.contains(handle.as_str()) {
            i += 1;
            handle = format!("group_{i}");
        }
        group_log!(
            "GroupSelection scope={:?} ids={node_ids:?} -> {handle:?}",
            self.scope
        );
        self.pending_actions.push(GraphEditCommand::GroupSelection {
            scope_path: self.scope.clone(),
            node_ids,
            handle,
            centroid: self.selection_centroid(),
        });
    }

    /// Emit an `Ungroup` action dissolving the selected group back into this
    /// level. Wired to Ctrl+Shift+G. No-op unless exactly one group node is
    /// selected.
    pub fn request_ungroup(&mut self) {
        let Some(group_id) = self.single_selected_group() else {
            return;
        };
        group_log!("Ungroup scope={:?} group={group_id}", self.scope);
        self.pending_actions.push(GraphEditCommand::Ungroup {
            scope_path: self.scope.clone(),
            group_id,
        });
    }

    /// Cycle the selected group's accent colour to the next preset tint, for
    /// legibility (Resolume / TouchDesigner colour-coding). No-op unless exactly
    /// one group node is selected. Emits `SetGroupTint`; the next colour is the
    /// one after the group's current tint in [`GROUP_TINT_PALETTE`] (or the
    /// first, when it's untinted or off-palette).
    pub fn request_cycle_group_tint(&mut self) {
        let Some(group_id) = self.single_selected_group() else {
            return;
        };
        let current = self
            .nodes
            .iter()
            .find(|n| n.id == group_id)
            .and_then(|n| n.group_tint);
        let next_idx = current
            .and_then(|c| GROUP_TINT_PALETTE.iter().position(|p| *p == c))
            .map(|i| (i + 1) % GROUP_TINT_PALETTE.len())
            .unwrap_or(0);
        group_log!("CycleGroupTint group={group_id} -> palette[{next_idx}]");
        self.pending_actions.push(GraphEditCommand::SetGroupTint {
            scope_path: self.scope.clone(),
            group_id,
            tint: Some(GROUP_TINT_PALETTE[next_idx]),
        });
    }

    /// The current view scope — a path of group node ids from the document
    /// root to the level being shown. Empty = root.
    /// Read by the app to scope graph edits (group/ungroup and per-node
    /// mutations) to the level the canvas is showing.
    pub fn scope_path(&self) -> &[u32] {
        &self.scope
    }

    /// Descend into a group node, showing its body as the canvas level. The
    /// next `set_snapshot` re-resolves and re-lays-out at the new level.
    /// No-op if the id isn't a group in the current view. Clears selection so
    /// a stale id from the parent level can't linger.
    pub(crate) fn enter_group(&mut self, group_id: u32) {
        let Some(node) = self.nodes.iter().find(|n| n.id == group_id) else {
            return;
        };
        if !node.is_group {
            return;
        }
        let title = node.title.clone();
        group_log!(
            "enter group {group_id} ({title:?}): scope {:?} -> depth {}",
            self.scope,
            self.scope.len() + 1
        );
        self.selected.clear();
        self.scope.push(group_id);
        self.scope_titles.push(title);
        // Auto-format this group the first time we open it (handled in the next
        // set_snapshot, and only if it has no saved layout).
        self.format_on_enter = true;
        // Frame the entered level once its nodes are laid out.
        self.fit_pending = true;
    }

    /// Pop one level back toward the root. Returns `true` if it moved (there
    /// was a level to leave), so the caller can mark the editor dirty. Clears
    /// selection for the same reason as `enter_group`.
    pub fn exit_group(&mut self) -> bool {
        if let Some(left) = self.scope.pop() {
            self.scope_titles.pop();
            group_log!("exit group {left}: scope now {:?}", self.scope);
            self.selected.clear();
            self.fit_pending = true;
            true
        } else {
            false
        }
    }

    /// Jump directly to a breadcrumb depth (0 = root, 1 = first group, …),
    /// truncating the scope path. Used by breadcrumb-bar clicks. No-op if the
    /// depth is already current or out of range.
    pub fn set_scope_depth(&mut self, depth: usize) {
        if depth < self.scope.len() {
            group_log!("breadcrumb jump to depth {depth}: {:?}", self.scope);
            self.scope.truncate(depth);
            self.scope_titles.truncate(depth);
            self.selected.clear();
            self.fit_pending = true;
        }
    }

    /// Toggle the debug overlay (scope/selection/hover/drag readout). Wired to
    /// the backtick key in the editor window.
    pub fn toggle_debug_overlay(&mut self) {
        self.debug_overlay = !self.debug_overlay;
        group_log!("debug overlay -> {}", self.debug_overlay);
    }

    /// Resolve a `(node_id, param_index)` (from a right-click) to the inner
    /// param's name. The app joins this with the snapshot's `node_handle` to
    /// look up the matching `UserParamBinding`.
    pub fn param_name_at(&self, node_id: u32, pi: usize) -> Option<String> {
        self.find_node(node_id)
            .and_then(|n| n.params.get(pi))
            .map(|p| p.name.clone())
    }

    /// Right-button press on the canvas. If it lands on an expanded
    /// param row, returns `(node_id, param_index)` so the app can resolve
    /// whether that inner param is exposed as a card binding and, if so,
    /// open the mapping popover via `open_mapping_popover`. Returns `None`
    /// for clicks that miss every param row (the app then leaves the
    /// canvas alone). A right-click anywhere first dismisses an open
    /// popover.
    pub fn on_right_button_down(&mut self, viewport: Rect, sx: f32, sy: f32) -> Option<(u32, usize)> {
        // A right-click outside the open popover dismisses it (and is
        // otherwise treated as a fresh hit-test).
        if self.mapping_popover.is_open() && !self.mapping_popover.contains_point(sx, sy) {
            self.mapping_popover.close();
        }
        self.param_row_under(viewport, sx, sy)
    }

    /// Open the mapping popover for a resolved binding, anchored on its
    /// param row. Called by the app after `on_right_button_down` reports
    /// a row AND the app has confirmed that row's inner param is exposed
    /// as a `UserParamBinding` (passing its current mapping in here). The
    /// canvas owns the anchor geometry; the app owns the binding lookup.
    #[allow(clippy::too_many_arguments)]
    pub fn open_mapping_popover(
        &mut self,
        viewport: Rect,
        node_id: u32,
        pi: usize,
        binding_id: String,
        label: String,
        min: f32,
        max: f32,
        invert: bool,
        curve: manifold_core::macro_bank::MacroCurve,
        scale: f32,
        offset: f32,
        range: Option<(f32, f32)>,
    ) {
        let Some(anchor) = self.param_row_rect(viewport, node_id, pi) else {
            return;
        };
        // Clip the popover to the canvas body (below the header strip).
        let clip = Rect::new(
            viewport.x,
            viewport.y + HEADER_HEIGHT,
            viewport.w,
            (viewport.h - HEADER_HEIGHT).max(0.0),
        );
        self.mapping_popover.open(
            binding_id, label, min, max, invert, curve, scale, offset, range, anchor, clip,
        );
    }

    /// Forward a left-button press to the open popover. Returns `true`
    /// when the popover consumed it (a handle/button hit, or any click
    /// inside the panel). A press outside the panel returns `false` and
    /// closes the popover, so the host can fall through to the normal
    /// canvas left-click path.
    pub fn popover_on_left_press(&mut self, sx: f32, sy: f32) -> bool {
        if !self.mapping_popover.is_open() {
            return false;
        }
        if self.mapping_popover.on_press(sx, sy) {
            true
        } else {
            self.mapping_popover.close();
            false
        }
    }

    /// Forward pointer motion to the open popover (drives the live range
    /// drag + handle hover). No-op when closed.
    pub fn popover_on_move(&mut self, sx: f32, sy: f32) {
        self.mapping_popover.on_move(sx, sy);
    }

    /// Forward a left-button release to the open popover (commits a range
    /// drag). No-op when closed.
    pub fn popover_on_left_release(&mut self) {
        self.mapping_popover.on_release();
    }

    /// `true` while the mapping popover is open. The host checks this so a
    /// left-click is routed to the popover first.
    pub fn popover_open(&self) -> bool {
        self.mapping_popover.is_open()
    }

    /// `true` while a popover value field is being typed into — the host routes
    /// keystrokes to it instead of firing canvas shortcuts.
    pub fn popover_is_editing(&self) -> bool {
        self.mapping_popover.is_editing()
    }

    /// Feed one typed character into the popover's active numeric field.
    pub fn popover_on_text_char(&mut self, c: char) {
        self.mapping_popover.on_text_char(c);
    }

    /// Delete the last typed character in the popover's active field.
    pub fn popover_on_backspace(&mut self) {
        self.mapping_popover.on_backspace();
    }

    /// Commit the popover's typed value (Enter).
    pub fn popover_commit_edit(&mut self) {
        self.mapping_popover.commit_edit();
    }

    /// Cancel the popover's numeric edit (Esc).
    pub fn popover_cancel_edit(&mut self) {
        self.mapping_popover.cancel_edit();
    }

    // ── Input handlers ──────────────────────────────────────────────

    pub fn on_pointer_move(&mut self, viewport: Rect, sx: f32, sy: f32) {
        self.cursor = (sx, sy);
        match &self.drag_mode {
            DragMode::Pan => {
                let dx = (sx - self.drag_anchor.0) / self.zoom;
                let dy = (sy - self.drag_anchor.1) / self.zoom;
                self.pan = (self.drag_pan_start.0 + dx, self.drag_pan_start.1 + dy);
            }
            DragMode::NodeMove {
                node_id,
                anchor_offset,
                ..
            } => {
                let nid = *node_id;
                let offset = *anchor_offset;
                let (gx, gy) = self.to_graph(viewport, sx, sy);
                if let Some(n) = self.nodes.iter_mut().find(|n| n.id == nid) {
                    n.pos_graph = (gx - offset.0, gy - offset.1);
                }
            }
            DragMode::WireFrom { .. } | DragMode::Marquee { .. } => {
                // Cursor position is enough — render reads `self.cursor` for
                // both the ghost wire and the live marquee rect.
            }
            DragMode::ParamScrub {
                node_id,
                param_name,
                range,
                start_value,
                is_int,
                press_origin_x,
            } => {
                let node_id = *node_id;
                let param_name = param_name.clone();
                let (min, max) = *range;
                let start_value = *start_value;
                let is_int = *is_int;
                let press_origin_x = *press_origin_x;
                let span = (max - min).max(f32::EPSILON);
                let delta_px = sx - press_origin_x;
                let mut v =
                    (start_value + delta_px * (span / PARAM_SCRUB_FULL_RANGE_PX)).clamp(min, max);
                if is_int {
                    v = v.round();
                }
                self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                    node_id,
                    param_name,
                    new_value: manifold_ui::SerializedParamValue::Float { value: v },
                });
            }
            DragMode::None => {
                self.hovered = self.node_under(viewport, sx, sy);
            }
        }
    }

    /// Begin panning unconditionally (e.g. middle-mouse drag).
    pub fn on_pan_button_down(&mut self, sx: f32, sy: f32) {
        self.drag_mode = DragMode::Pan;
        self.drag_anchor = (sx, sy);
        self.drag_pan_start = self.pan;
    }

    pub fn on_pan_button_up(&mut self) {
        if self.drag_mode.is_pan() {
            self.drag_mode = DragMode::None;
        }
    }

    /// Left-button press dispatch. `shift` toggles selection / box-select.
    /// Resolves, in priority order: breadcrumb bar → reset button → collapse
    /// chevron → port (wire/disconnect) → param-row scrub → group double-click
    /// → header drag → node select → empty (picker / marquee / pan).
    pub fn on_left_button_down(
        &mut self,
        viewport: Rect,
        sx: f32,
        sy: f32,
        now: f32,
        shift: bool,
    ) {
        // Breadcrumb bar (header chrome) — jump to a shallower scope. Gets
        // first crack like the reset button since it sits above the canvas
        // surface. No-op return value means the click wasn't on a crumb.
        if let Some(depth) = self.breadcrumb_hit(viewport, sx, sy) {
            self.set_scope_depth(depth);
            return;
        }
        // Header button has priority over everything else — it sits in
        // the chrome above the canvas surface.
        if self.has_graph_mod {
            let rect = self.reset_button_rect(viewport);
            if sx >= rect.x && sx <= rect.x + rect.w && sy >= rect.y && sy <= rect.y + rect.h {
                self.pending_actions.push(GraphEditCommand::RevertEffectGraph);
                return;
            }
        }
        // Collapse chevron in a node header toggles that node's param rows.
        // Checked before ports/header so it doesn't start a wire or a move.
        if let Some(node_id) = self.chevron_under(viewport, sx, sy) {
            let now = !self.collapsed.get(&node_id).copied().unwrap_or(true);
            self.collapsed.insert(node_id, now);
            if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
                node.collapsed = now;
            }
            return;
        }
        if let Some(hit) = self.port_under(viewport, sx, sy) {
            if hit.is_output {
                self.drag_mode = DragMode::WireFrom {
                    from_node: hit.node_id,
                    from_port: hit.port_name,
                };
                return;
            }
            // Input port — if a wire feeds this port, breaking it on
            // click. Otherwise swallow so the click doesn't start a pan.
            if self.wire_into(hit.node_id, &hit.port_name).is_some() {
                self.pending_actions.push(GraphEditCommand::DisconnectPorts {
                    to_node: hit.node_id,
                    to_port: hit.port_name,
                });
            }
            return;
        }
        // Param row on the node face → start a value scrub for numeric
        // params with a range; for non-scrubbable params just select the
        // node so the inspector sidebar can edit them.
        if let Some((node_id, pi)) = self.param_row_under(viewport, sx, sy) {
            let info = self
                .nodes
                .iter()
                .find(|n| n.id == node_id)
                .and_then(|n| n.params.get(pi).map(|p| (p.name.clone(), p.scrub)));
            if let Some((param_name, scrub)) = info {
                self.select_single(node_id);
                if let Some(s) = scrub {
                    self.drag_mode = DragMode::ParamScrub {
                        node_id,
                        param_name,
                        range: s.range,
                        start_value: s.current_value,
                        is_int: s.is_int,
                        press_origin_x: sx,
                    };
                }
                return;
            }
        }
        // Double-click on a group node descends into it. Checked before the
        // header-drag path so entering doesn't also start a move; a single
        // click on a group falls through to select / header-drag below.
        if let Some(node_id) = self.node_under(viewport, sx, sy) {
            let is_group = self
                .nodes
                .iter()
                .find(|n| n.id == node_id)
                .is_some_and(|n| n.is_group);
            if is_group {
                let dbl = self.is_double_click(sx, sy, now, Some(node_id));
                self.note_click(sx, sy, now, Some(node_id));
                if dbl {
                    self.last_click_time = None; // latch so a 3rd press is fresh
                    self.enter_group(node_id);
                    return;
                }
            }
        }
        if let Some(node_id) = self.header_under(viewport, sx, sy) {
            self.click_select(node_id, shift);
            let (gx, gy) = self.to_graph(viewport, sx, sy);
            if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
                let anchor_offset = (gx - node.pos_graph.0, gy - node.pos_graph.1);
                self.drag_mode = DragMode::NodeMove {
                    node_id,
                    anchor_offset,
                    start_pos: node.pos_graph,
                };
            }
            return;
        }
        match self.node_under(viewport, sx, sy) {
            Some(id) => {
                self.click_select(id, shift);
            }
            None => {
                // Double-click on empty space opens the node picker at the
                // cursor. Two presses on empty space within the time +
                // distance window count as a double-click.
                let is_double = self.is_double_click(sx, sy, now, None);
                self.note_click(sx, sy, now, None);
                if is_double {
                    // Latch reset so a third press doesn't triple-fire.
                    self.last_click_time = None;
                    let (gx, gy) = self.to_graph(viewport, sx, sy);
                    self.pending_actions.push(GraphEditCommand::OpenNodePicker {
                        screen_pos: (sx, sy),
                        graph_pos: (gx, gy),
                    });
                } else if shift {
                    // Shift+drag = rubber-band box select (replaces the
                    // selection with whatever the box covers). A shift-press
                    // with no drag is a no-op (guarded on release).
                    self.drag_mode = DragMode::Marquee {
                        origin_screen: (sx, sy),
                    };
                } else {
                    // Plain left-drag = pan, so the canvas stays navigable on a
                    // trackpad. A left-click with no drag clears the selection
                    // (handled on release).
                    self.drag_mode = DragMode::Pan;
                    self.drag_anchor = (sx, sy);
                    self.drag_pan_start = self.pan;
                }
            }
        }
    }

    /// Record a left-press for double-click detection. `node` is the node id
    /// under the press (`None` for empty space).
    pub(crate) fn note_click(&mut self, sx: f32, sy: f32, now: f32, node: Option<u32>) {
        self.last_click_time = Some(now);
        self.last_click_pos = (sx, sy);
        self.last_click_node = node;
    }

    /// True when the press at `(sx, sy, now)` over `node` completes a
    /// double-click of the previous press: same target, within the time and
    /// distance window.
    pub(crate) fn is_double_click(&self, sx: f32, sy: f32, now: f32, node: Option<u32>) -> bool {
        let dx = sx - self.last_click_pos.0;
        let dy = sy - self.last_click_pos.1;
        self.last_click_time
            .is_some_and(|t| now - t < DOUBLE_CLICK_SECONDS)
            && (dx * dx + dy * dy) < DOUBLE_CLICK_RADIUS_PX * DOUBLE_CLICK_RADIUS_PX
            && self.last_click_node == node
    }

    /// Apply a node click to the selection set. Shift toggles membership; a
    /// plain click on an unselected node selects just it; a plain click on an
    /// already-selected node leaves the (possibly multi-) selection intact so
    /// it can be dragged as a group.
    fn click_select(&mut self, id: u32, shift: bool) {
        if shift {
            if !self.selected.insert(id) {
                self.selected.remove(&id);
            }
        } else if !self.selected.contains(&id) {
            self.selected.clear();
            self.selected.insert(id);
        }
    }

    /// Replace the selection with exactly `id`. Used where multi-select
    /// doesn't apply (param-row focus).
    pub(crate) fn select_single(&mut self, id: u32) {
        self.selected.clear();
        self.selected.insert(id);
    }

    /// The selected node ids at the current scope, sorted for stable command
    /// payloads. Read by Layer 3's Ctrl+G to build the group selection.
    pub fn selected_ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.selected.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// If exactly one node is selected and it's a group, its id — for
    /// Ctrl+Shift+G ungroup. `None` otherwise.
    pub fn single_selected_group(&self) -> Option<u32> {
        if self.selected.len() != 1 {
            return None;
        }
        let id = *self.selected.iter().next()?;
        self.nodes
            .iter()
            .find(|n| n.id == id && n.is_group)
            .map(|n| n.id)
    }

    /// Graph-space centroid of the current selection — the natural drop point
    /// for a new group node. Falls back to the layout origin when empty.
    pub fn selection_centroid(&self) -> (f32, f32) {
        let mut sx = 0.0;
        let mut sy = 0.0;
        let mut n = 0.0;
        for node in self.nodes.iter().filter(|nv| self.selected.contains(&nv.id)) {
            sx += node.pos_graph.0;
            sy += node.pos_graph.1;
            n += 1.0;
        }
        if n > 0.0 {
            (sx / n, sy / n)
        } else {
            LAYOUT_ORIGIN
        }
    }

    pub fn on_left_button_up(&mut self, viewport: Rect, sx: f32, sy: f32) {
        let prev = std::mem::replace(&mut self.drag_mode, DragMode::None);
        match prev {
            DragMode::None => {}
            DragMode::Pan => {
                // A left-press that didn't actually pan (cursor barely moved) is
                // a click on empty space — clear the selection. A real pan
                // leaves the selection alone.
                let moved = (sx - self.drag_anchor.0).hypot(sy - self.drag_anchor.1);
                if moved < CLICK_MOVE_SLOP_PX {
                    self.selected.clear();
                }
            }
            DragMode::WireFrom {
                from_node,
                from_port,
            } => {
                // Only commit on drop over an input port — drop on
                // empty or an output cancels silently.
                if let Some(hit) = self.port_under(viewport, sx, sy)
                    && !hit.is_output
                    && hit.node_id != from_node
                {
                    self.pending_actions.push(GraphEditCommand::ConnectPorts {
                        from_node,
                        from_port,
                        to_node: hit.node_id,
                        to_port: hit.port_name,
                    });
                }
            }
            DragMode::NodeMove { node_id, .. } => {
                if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
                    self.pending_actions.push(GraphEditCommand::MoveGraphNode {
                        node_id,
                        new_pos: node.pos_graph,
                    });
                }
            }
            // The scrub emitted its value on each pointer move; nothing to
            // finalize on release.
            DragMode::ParamScrub { .. } => {}
            DragMode::Marquee { origin_screen } => {
                // A shift-press with no real drag leaves the selection alone —
                // don't let a zero-area box wipe it.
                let (ox, oy) = origin_screen;
                if (sx - ox).hypot(sy - oy) < CLICK_MOVE_SLOP_PX {
                    return;
                }
                // Build the graph-space rect from press to release; the nodes
                // it intersects become the selection (replace).
                let (gx0, gy0) = self.to_graph(viewport, ox.min(sx), oy.min(sy));
                let (gx1, gy1) = self.to_graph(viewport, ox.max(sx), oy.max(sy));
                let rect = (gx0, gy0, gx1 - gx0, gy1 - gy0);
                self.selected = marquee_hits(rect, &self.nodes).into_iter().collect();
                group_log!(
                    "marquee commit: {} node(s) selected {:?}",
                    self.selected.len(),
                    self.selected
                );
            }
        }
    }

    pub fn cursor(&self) -> (f32, f32) {
        self.cursor
    }

    /// The single focused node id, or `None` when zero or several are
    /// selected. Read by the editor's right-sidebar panel to figure out which
    /// inner-node parameters to show as expose checkboxes — that surface only
    /// makes sense for one node, so a multi-selection reports `None`.
    pub fn selected_node_id(&self) -> Option<u32> {
        if self.selected.len() == 1 {
            self.selected.iter().copied().next()
        } else {
            None
        }
    }
}
