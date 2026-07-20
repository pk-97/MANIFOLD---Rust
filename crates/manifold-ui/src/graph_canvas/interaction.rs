//! Canvas interaction: the `CanvasDrag` drag-lifecycle payload, the per-event
//! input handlers the editor window calls (pointer/pan/left-button),
//! selection and scope navigation, the mapping-popover forwarders, and the
//! `request_*` command emitters. Every mutation is queued onto
//! `pending_actions` and drained by the editor window each event.

use super::*;

/// The canvas's one drag-lifecycle payload (P7.2,
/// `docs/UI_WIDGET_UNIFICATION_DESIGN.md` D8/D9). Was `DragMode` — renamed so
/// the compiler-driven sweep touched every site (D9). Session geometry
/// replaces the per-variant position fields: `press_origin_x` is
/// `session.start.x`; `Marquee`'s `origin_screen` is `session.start`; `Pan`'s
/// `drag_anchor` is `session.start` with `drag_pan_start` captured at grab as
/// `pan_at_grab`. Idle is the controller's `None` session — there is no
/// `None` variant here.
#[derive(Debug, Clone)]
pub(crate) enum CanvasDrag {
    /// Panning the canvas. `pan_at_grab` is `self.pan` at press time — was
    /// the field `drag_pan_start`.
    Pan { pan_at_grab: (f32, f32) },
    /// Dragging from an output port to draw a wire. On release over an
    /// input port, emits `GraphEditCommand::ConnectPorts`.
    WireFrom {
        from_node: u32,
        from_port: String,
    },
    /// Dragging a node by its header. `anchor_offset` is the graph-space
    /// (cursor - node_origin) at button-down so the node doesn't snap
    /// to the cursor on pickup. The undo previous-pos doesn't need a
    /// pre-drag snapshot here: `MoveGraphNodeCommand::execute` captures
    /// `node.editor_pos` itself before overwriting it.
    NodeMove {
        node_id: u32,
        anchor_offset: (f32, f32),
    },
    /// Scrubbing a numeric param on a node's face. Cumulative pixel delta
    /// from the grab position (`session.start.x`) maps to a value delta over
    /// `PARAM_SCRUB_FULL_RANGE_PX`, anchored on `start_value` so a long
    /// drag doesn't accumulate float error. Emits `SetGraphNodeParam` each
    /// pointer move, matching the inspector sidebar — unless `outer_param_id`
    /// is `Some`, meaning this row is a group-face mirror (D6): the drag then
    /// emits `GraphEditCommand::SetOuterParam` instead, the card's own write
    /// path (the parity invariant), and `node_id`/`param_name` are unused for
    /// addressing (kept only so the group box stays the visual selection).
    ParamScrub {
        node_id: u32,
        param_name: String,
        range: (f32, f32),
        start_value: f32,
        is_int: bool,
        outer_param_id: Option<String>,
    },
    /// Scrubbing one channel of a `Color` / `Vec2..4` param via its row in the
    /// open [`VecEditor`](super::VecEditor) panel. `base` is the whole vector at
    /// press; the dragged `channel` is overwritten each pointer move and the full
    /// vector emitted as one `SetGraphNodeParam` (the other channels held) —
    /// parity with the sidebar's channel scrub. Colours scrub 0..1; vectors over
    /// their declared range. Anchored on `start_value = base[channel]` via the
    /// grab position (`session.start.x`) so a long drag doesn't accumulate
    /// float error.
    VecScrub {
        node_id: u32,
        param_name: String,
        kind: crate::graph_view::ParamSnapshotKind,
        channel: usize,
        base: [f32; 4],
        range: (f32, f32),
    },
    /// Rubber-band selection from a Shift+empty-canvas press. The press point
    /// is `session.start` (was the field `origin_screen`); the live rect
    /// spans it to the current cursor. On release, the nodes the box
    /// intersects become the selection (replace).
    Marquee,
}

impl CanvasDrag {
    /// Short tag for the debug overlay readout. The idle case (was
    /// `DragMode::None`) lives at the readout call site now — there's no
    /// idle variant to match here.
    pub(crate) fn debug_label(&self) -> &'static str {
        match self {
            CanvasDrag::Pan { .. } => "pan",
            CanvasDrag::WireFrom { .. } => "wire",
            CanvasDrag::NodeMove { .. } => "node-move",
            CanvasDrag::ParamScrub { .. } => "param-scrub",
            CanvasDrag::VecScrub { .. } => "vec-scrub",
            CanvasDrag::Marquee => "marquee",
        }
    }
}

/// Pick the `SerializedParamValue` for a multi-component param kind from a full
/// RGBA/XYZW vector (extra tail components dropped for `Vec2`/`Vec3`). `None` for
/// non-vec kinds. Mirrors the sidebar's channel-scrub emit, so an on-node and a
/// sidebar colour/vector edit produce a byte-identical command.
fn vec_serialized(
    kind: crate::graph_view::ParamSnapshotKind,
    full: [f32; 4],
) -> Option<crate::SerializedParamValue> {
    use crate::SerializedParamValue as V;
    use crate::graph_view::ParamSnapshotKind as K;
    Some(match kind {
        K::Color => V::Color { value: full },
        K::Vec4 => V::Vec4 { value: full },
        K::Vec3 => V::Vec3 {
            value: [full[0], full[1], full[2]],
        },
        K::Vec2 => V::Vec2 {
            value: [full[0], full[1]],
        },
        _ => return None,
    })
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

    /// Emit `AddGraphNodeAt` for a node picked via keyboard (Enter /
    /// type-and-enter) in the editor's node picker. The picker popup itself
    /// lives on the editor window's `UIRoot`, not the canvas — but the
    /// resulting spawn is a `GraphEditCommand`, so it queues here the same
    /// way a mouse-click pick does (`app_render.rs`'s
    /// `BrowserPopupAction::NodeSelected` arm), just reached from
    /// `window_input.rs`'s keyboard handler instead of a raw click event.
    pub fn request_add_node_at(&mut self, type_id: String, graph_pos: (f32, f32)) {
        self.pending_actions
            .push(GraphEditCommand::AddGraphNodeAt { type_id, graph_pos });
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
            // The command/def store the tint as a plain sRGB float array; the
            // palette is sRGB `Color32`. Convert at this boundary (no gamma).
            tint: Some(GROUP_TINT_PALETTE[next_idx].to_srgb_f32()),
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
    ///
    /// mirrors the card slider's own hit-zone split — a
    /// right-click on a numeric ranged param row's TRACK zone (right of the
    /// label cell, same boundary `param_slider_track_x` sources from
    /// `BitmapSlider::zones()`) resets that param to its default in place
    /// (`SliderIntent::ResetToDefault`, resolved via `intent_for`), exactly
    /// like every card/panel slider (D4: the same absolute-set command shape
    /// the scrub-commit path emits). The LABEL zone is unaffected and keeps
    /// falling through to the mapping-popover path below (D3: this host has
    /// no translation for `OpenMapping` here — the existing popover path
    /// covers it independently). Wire-driven rows are skipped — read-only,
    /// same guard the scrub uses.
    pub fn on_right_button_down(&mut self, viewport: Rect, sx: f32, sy: f32) -> Option<(u32, usize)> {
        // A right-click outside the open popover dismisses it (and is
        // otherwise treated as a fresh hit-test).
        if self.mapping_popover.is_open() && !self.mapping_popover.contains_point(sx, sy) {
            self.mapping_popover.close();
        }
        let hit = self.param_row_under(viewport, sx, sy)?;
        let (node_id, pi) = hit;
        let track_hit = self.param_slider_track_x(viewport, node_id).is_some_and(|track_x| sx >= track_x);
        let wants_reset = track_hit
            && matches!(
                crate::slider::BitmapSlider::intent_for(
                    crate::slider::SliderZone::Track,
                    crate::intent::Gesture::RightClick,
                ),
                Some(crate::slider::SliderIntent::ResetToDefault)
            );
        if wants_reset
            && let Some(p) = self.find_node(node_id).and_then(|n| n.params.get(pi).cloned())
            && p.scrub.is_some()
            && !p.wire_driven
        {
            // D4: the same absolute-set command shape the scrub-commit path
            // emits, carrying `default_value` — undo == a drag to default.
            if let Some(outer_param_id) = p.outer_param_id {
                self.pending_actions.push(GraphEditCommand::SetOuterParam {
                    outer_param_id,
                    new_value: p.default_value,
                });
            } else {
                self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                    node_id,
                    param_name: p.name,
                    new_value: crate::SerializedParamValue::Float {
                        value: p.default_value,
                    },
                });
            }
            return None;
        }
        Some(hit)
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
        curve: crate::MacroCurve,
        scale: f32,
        offset: f32,
        range: Option<(f32, f32)>,
        section: Option<String>,
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
            binding_id, label, min, max, invert, curve, scale, offset, range, section, anchor,
            clip,
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
        let pos = crate::node::Vec2::new(sx, sy);
        let Some(session) = self.drag.track(pos) else {
            self.hovered = self.node_under(viewport, sx, sy);
            return;
        };
        // Session geometry replaces the old per-variant position fields (D9):
        // `press_origin_x` is `session.start.x`.
        let press_origin_x = session.start.x;
        match &session.payload {
            CanvasDrag::Pan { pan_at_grab } => {
                let dx = (sx - session.start.x) / self.zoom;
                let dy = (sy - session.start.y) / self.zoom;
                self.pan = (pan_at_grab.0 + dx, pan_at_grab.1 + dy);
            }
            CanvasDrag::NodeMove {
                node_id,
                anchor_offset,
                ..
            } => {
                let nid = *node_id;
                let offset = *anchor_offset;
                let (gx, gy) = self.to_graph(viewport, sx, sy);
                if let Some(n) = self.nodes.iter_mut().find(|n| n.id == nid) {
                    n.pos_graph = (
                        snap_to_grid(gx - offset.0),
                        snap_to_grid(gy - offset.1),
                    );
                }
            }
            CanvasDrag::WireFrom { .. } | CanvasDrag::Marquee => {
                // Cursor position is enough — render reads `self.cursor` for
                // both the ghost wire and the live marquee rect.
            }
            CanvasDrag::ParamScrub {
                node_id,
                param_name,
                range,
                start_value,
                is_int,
                outer_param_id,
            } => {
                let node_id = *node_id;
                let param_name = param_name.clone();
                let (min, max) = *range;
                let start_value = *start_value;
                let is_int = *is_int;
                let outer_param_id = outer_param_id.clone();
                let span = (max - min).max(f32::EPSILON);
                let delta_px = sx - press_origin_x;
                let mut v =
                    (start_value + delta_px * (span / PARAM_SCRUB_FULL_RANGE_PX)).clamp(min, max);
                if is_int {
                    v = v.round();
                }
                // D6 parity invariant: a group-face row scrubs the exact same
                // outer card param, through the card's own write path — never
                // the inner node's `SetGraphNodeParam` addressing.
                if let Some(outer_param_id) = outer_param_id {
                    self.pending_actions.push(GraphEditCommand::SetOuterParam {
                        outer_param_id,
                        new_value: v,
                    });
                } else {
                    self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                        node_id,
                        param_name,
                        new_value: crate::SerializedParamValue::Float { value: v },
                    });
                }
            }
            CanvasDrag::VecScrub {
                node_id,
                param_name,
                kind,
                channel,
                base,
                range,
            } => {
                let node_id = *node_id;
                let param_name = param_name.clone();
                let kind = *kind;
                let channel = *channel;
                let base = *base;
                let (min, max) = *range;
                let span = (max - min).max(f32::EPSILON);
                let delta_px = sx - press_origin_x;
                let v =
                    (base[channel] + delta_px * (span / PARAM_SCRUB_FULL_RANGE_PX)).clamp(min, max);
                // Overwrite the dragged channel; emit the whole colour/vector so
                // the other channels carry through unchanged (sidebar parity).
                let mut full = base;
                full[channel] = v;
                if let Some(new_value) = vec_serialized(kind, full) {
                    self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                        node_id,
                        param_name,
                        new_value,
                    });
                }
            }
        }
    }

    /// Begin panning unconditionally (e.g. middle-mouse drag).
    pub fn on_pan_button_down(&mut self, sx: f32, sy: f32) {
        self.drag.start(
            CanvasDrag::Pan { pan_at_grab: self.pan },
            crate::node::Vec2::new(sx, sy),
        );
    }

    pub fn on_pan_button_up(&mut self) {
        if matches!(self.drag.payload(), Some(CanvasDrag::Pan { .. })) {
            self.drag.cancel();
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
        // An open enum dropdown is modal over the canvas — it gets first crack.
        // Any press closes it (taken here); a press on an option sets the value,
        // a press elsewhere just dismisses and falls through to normal handling.
        if let Some(dd) = self.enum_dropdown.take() {
            if let Some(idx) = dd.option_at(sx, sy) {
                if idx != dd.current {
                    if let Some(outer_param_id) = dd.outer_param_id.clone() {
                        self.pending_actions.push(GraphEditCommand::SetOuterParam {
                            outer_param_id,
                            new_value: idx as f32,
                        });
                    } else {
                        self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                            node_id: dd.node_id,
                            param_name: dd.param_name.clone(),
                            new_value: crate::SerializedParamValue::Enum { value: idx as u32 },
                        });
                    }
                }
                return;
            }
            if dd.contains(sx, sy) {
                return; // pressed the list edge — swallow, stay closed
            }
            // Pressed outside the list: dismissed, continue with normal dispatch.
        }
        // An open Color/Vec editor is modal like the enum dropdown, but its rows
        // are draggable: a press on a channel row starts a scrub (the panel stays
        // open so you can grab another channel), a press elsewhere inside swallows,
        // and a press outside dismisses and falls through to normal handling.
        if self.vec_editor.is_some() {
            let (ch, inside, node_id, param_name, is_color) = {
                let ed = self.vec_editor.as_ref().unwrap();
                (
                    ed.channel_at(sx, sy),
                    ed.contains(sx, sy),
                    ed.node_id,
                    ed.param_name.clone(),
                    ed.is_color,
                )
            };
            if let Some(ch) = ch {
                // Seed the drag from the param's live vector + range at press.
                if let Some((base, range, kind)) = self
                    .find_node(node_id)
                    .and_then(|n| n.params.iter().find(|p| p.name == param_name))
                    .map(|p| {
                        let range = if is_color {
                            (0.0, 1.0)
                        } else {
                            p.range.unwrap_or((-1.0, 1.0))
                        };
                        (p.vec_value, range, p.kind)
                    })
                {
                    self.drag.start(
                        CanvasDrag::VecScrub {
                            node_id,
                            param_name,
                            kind,
                            channel: ch,
                            base,
                            range,
                        },
                        crate::node::Vec2::new(sx, sy),
                    );
                }
                return;
            }
            if inside {
                return; // header / padding — swallow, stay open
            }
            // Pressed outside the panel: dismiss and continue with normal dispatch.
            self.vec_editor = None;
        }
        // An open Table grid editor is modal like the others: a press on a cell
        // opens that cell's inline numeric editor (`EditGraphNodeTableCell`) and
        // keeps the grid open so you can edit more cells; a press elsewhere inside
        // swallows; a press outside dismisses and falls through.
        if self.table_editor.is_some() {
            let (cell_rect, inside, node_id, param_name) = {
                let ed = self.table_editor.as_ref().unwrap();
                (
                    ed.cell_at(sx, sy).map(|(r, c)| (r, c, ed.cell_rect(r, c))),
                    ed.contains(sx, sy),
                    ed.node_id,
                    ed.param_name.clone(),
                )
            };
            if let Some((r, c, rect)) = cell_rect {
                // Read the whole live table off the node so the commit can rebuild
                // just this cell into a full `Table` value (sidebar parity).
                if let Some(rows) = self
                    .find_node(node_id)
                    .and_then(|n| n.params.iter().find(|p| p.name == param_name))
                    .and_then(|p| p.table_value.clone())
                {
                    let current = rows.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0.0);
                    self.pending_actions.push(GraphEditCommand::EditGraphNodeTableCell {
                        node_id,
                        param_name,
                        row: r,
                        col: c,
                        current,
                        rows,
                        anchor: (rect.x, rect.y, rect.w, rect.h),
                    });
                }
                return;
            }
            if inside {
                return; // header line / padding — swallow, stay open
            }
            self.table_editor = None;
        }
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
        // Save to Library / Save to Project header pills (PRESET_LIBRARY_DESIGN
        // D4, P3) — same chrome-priority as Reset; only present when there's an
        // active graph (mirrors the render-side gate).
        if !self.nodes.is_empty() {
            let sp_rect = self.save_to_project_button_rect(viewport);
            if sx >= sp_rect.x && sx <= sp_rect.x + sp_rect.w && sy >= sp_rect.y && sy <= sp_rect.y + sp_rect.h {
                self.pending_actions.push(GraphEditCommand::SaveGraphToProject {
                    anchor: (sp_rect.x, sp_rect.y, sp_rect.w, sp_rect.h),
                });
                return;
            }
            let sl_rect = self.save_to_library_button_rect(viewport);
            if sx >= sl_rect.x && sx <= sl_rect.x + sl_rect.w && sy >= sl_rect.y && sy <= sl_rect.y + sl_rect.h {
                self.pending_actions.push(GraphEditCommand::SaveGraphToLibrary {
                    anchor: (sl_rect.x, sl_rect.y, sl_rect.w, sl_rect.h),
                });
                return;
            }
        }
        // Push to Library header pill (PRESET_LIBRARY_DESIGN D3, P4) — same
        // chrome-priority, only present while diverged (mirrors the
        // render-side gate).
        if self.has_graph_mod {
            let pl_rect = self.push_to_library_button_rect(viewport);
            if sx >= pl_rect.x && sx <= pl_rect.x + pl_rect.w && sy >= pl_rect.y && sy <= pl_rect.y + pl_rect.h {
                self.pending_actions.push(GraphEditCommand::PushGraphToLibrary {
                    anchor: (pl_rect.x, pl_rect.y, pl_rect.w, pl_rect.h),
                });
                return;
            }
        }
        // No collapse toggle — nodes stay expanded (Blender-style). A header
        // click falls through to select / drag below.
        if let Some(hit) = self.port_under(viewport, sx, sy) {
            if hit.is_output {
                self.drag.start(
                    CanvasDrag::WireFrom {
                        from_node: hit.node_id,
                        from_port: hit.port_name,
                    },
                    crate::node::Vec2::new(sx, sy),
                );
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
        // "+ Object" / "+ Light" gesture buttons on `render_scene`'s face
        // (D7/D7a, `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md` §2). Checked
        // before the expose glyph / param scrub below — Action rows carry no
        // param so `param_row_under` already skips them, but checking first
        // keeps the gesture's dispatch next to the other whole-row hit-tests
        // (port drag, header chrome) rather than buried past the param path.
        if let Some((node_id, kind)) = self.action_row_under(viewport, sx, sy) {
            self.select_single(node_id);
            if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
                let scope_path = self.scope.clone();
                let (nx, ny) = node.pos_graph;
                match kind {
                    NodeActionKind::AddSceneObject => {
                        let next_index = node
                            .params
                            .iter()
                            .find(|p| p.name == "objects")
                            .map(|p| p.current_value.round().max(0.0) as u32)
                            .unwrap_or(0);
                        // Stagger successive object groups below-right of
                        // render_scene so repeated clicks don't stack new
                        // boxes exactly on top of each other; drag to taste.
                        let centroid = (nx + NODE_WIDTH + 80.0, ny + next_index as f32 * 220.0);
                        self.pending_actions.push(GraphEditCommand::AddSceneObject {
                            scope_path,
                            render_scene_node_id: node_id,
                            next_index,
                            centroid,
                        });
                    }
                    NodeActionKind::AddSceneLight => {
                        let next_index = node
                            .params
                            .iter()
                            .find(|p| p.name == "lights")
                            .map(|p| p.current_value.round().max(0.0) as u32)
                            .unwrap_or(0);
                        let pos = (nx - 260.0, ny + next_index as f32 * 140.0);
                        self.pending_actions.push(GraphEditCommand::AddSceneLight {
                            scope_path,
                            render_scene_node_id: node_id,
                            next_index,
                            pos,
                        });
                    }
                }
            }
            return;
        }
        // Expose glyph at a param row's left edge → toggle whether the inner
        // param feeds the outer performance card (Blender-style on-node expose).
        // Checked before the row scrub so a click on the dot exposes rather than
        // scrubbing. Consumes the click either way (even for a handle-less node
        // that can't be exposed), so the dot never falls through to a scrub.
        if let Some((node_id, pi)) = self.expose_glyph_under(viewport, sx, sy) {
            self.select_single(node_id);
            // A wire-driven param is read-only: the wire feeds it every frame, so
            // exposing it to the card would lie about what drives it. Consume the
            // click (it selected the node) but emit no toggle — parity with the
            // sidebar's disabled checkbox.
            let wire_driven = self
                .find_node(node_id)
                .and_then(|n| n.params.get(pi))
                .is_some_and(|p| p.wire_driven);
            if !wire_driven && let Some(cmd) = self.build_expose_toggle(node_id, pi) {
                self.pending_actions.push(cmd);
            }
            return;
        }
        // Param row on the node face. Numeric params with a range start a value
        // scrub; the rest open an editor keyed to the param kind — a discrete
        // edit (bool flip / trigger fire), or a floating panel (enum dropdown,
        // colour/vec channels, table grid), or the app's inline text / native
        // folder editor (string / path). Every branch emits the same command the
        // sidebar did (parity); only where you click moves.
        if let Some((node_id, pi)) = self.param_row_under(viewport, sx, sy) {
            let pv = self
                .nodes
                .iter()
                .find(|n| n.id == node_id)
                .and_then(|n| n.params.get(pi).cloned());
            if let Some(p) = pv {
                self.select_single(node_id);
                // Wire-driven params are read-only: a same-named input wire feeds
                // the value every frame, so a scrub / editor here would fight the
                // wire and lie about control. Consume the click (it selected the
                // node) but open nothing — remove the wire to reclaim the param.
                // Instead, highlight the feeding wire (D5) — the click still does
                // *something* legible: it points at what's actually driving the
                // row.
                if p.wire_driven {
                    self.highlighted_wire = self
                        .wires
                        .iter()
                        .find(|w| w.to_node == node_id && w.to_port == p.name)
                        .map(|w| (w.to_node, w.to_port.clone()));
                    return;
                }
                // Numeric ranged params scrub in place — UNLESS this press is
                // the second half of a double-click landing in the value-cell
                // zone (D13's `(ValueCell, DoubleClick) -> EditValue` row,
                // P5d — the contract's last dead stop). That opens the
                // numeric type-in instead of arming a scrub; single-clicks
                // anywhere else on the row keep scrubbing exactly as before.
                if let Some(s) = p.scrub {
                    let in_value_cell = self.param_slider_zones(viewport, node_id).is_some_and(|z| {
                        sx >= z.value_cell.x && sx <= z.value_cell.x + z.value_cell.width
                    });
                    let wants_edit = in_value_cell
                        && self.is_double_click(sx, sy, now, Some(node_id))
                        && matches!(
                            crate::slider::BitmapSlider::intent_for(
                                crate::slider::SliderZone::ValueCell,
                                crate::intent::Gesture::DoubleClick,
                            ),
                            Some(crate::slider::SliderIntent::EditValue)
                        );
                    self.note_click(sx, sy, now, Some(node_id));
                    if wants_edit {
                        self.last_click_time = None; // latch so a 3rd press is fresh
                        if let Some(anchor) = self.param_row_rect(viewport, node_id, pi) {
                            self.pending_actions.push(GraphEditCommand::EditGraphNodeNumericParam {
                                node_id,
                                param_name: p.name,
                                current: s.current_value,
                                min: s.range.0,
                                max: s.range.1,
                                whole_numbers: s.is_int,
                                outer_param_id: p.outer_param_id,
                                anchor: (anchor.x, anchor.y, anchor.w, anchor.h),
                            });
                        }
                        return;
                    }
                    self.drag.start(
                        CanvasDrag::ParamScrub {
                            node_id,
                            param_name: p.name,
                            range: s.range,
                            start_value: s.current_value,
                            is_int: s.is_int,
                            outer_param_id: p.outer_param_id,
                        },
                        crate::node::Vec2::new(sx, sy),
                    );
                    return;
                }
                // A Table param (`kind` is `Other`, carries `table_value`) opens
                // the grid editor anchored on the row; clicking a cell emits
                // `EditGraphNodeTableCell`.
                if let Some(rows) = p.table_value.as_ref() {
                    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                    if !rows.is_empty()
                        && cols > 0
                        && let Some(anchor) = self.param_row_rect(viewport, node_id, pi)
                    {
                        self.enum_dropdown = None;
                        self.vec_editor = None;
                        self.table_editor = Some(super::TableEditor {
                            node_id,
                            param_name: p.name,
                            rows: rows.len(),
                            cols,
                            anchor,
                        });
                    }
                    return;
                }
                use crate::graph_view::ParamSnapshotKind as K;
                match p.kind {
                    // Bool → flip; Trigger → fire (+1). Both normally emit an
                    // absolute SetGraphNodeParam (parity with the sidebar's
                    // value cell) — unless this is a group-face mirror row
                    // (D6), which emits SetOuterParam instead (the card's own
                    // write path, the parity invariant).
                    K::Bool => {
                        let new_value = if p.current_value < 0.5 { 1.0 } else { 0.0 };
                        if let Some(outer_param_id) = p.outer_param_id {
                            self.pending_actions.push(GraphEditCommand::SetOuterParam {
                                outer_param_id,
                                new_value,
                            });
                        } else {
                            self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                                node_id,
                                param_name: p.name,
                                new_value: crate::SerializedParamValue::Bool {
                                    value: new_value >= 0.5,
                                },
                            });
                        }
                    }
                    K::Trigger => {
                        let new_value = p.current_value + 1.0;
                        if let Some(outer_param_id) = p.outer_param_id {
                            self.pending_actions.push(GraphEditCommand::SetOuterParam {
                                outer_param_id,
                                new_value,
                            });
                        } else {
                            self.pending_actions.push(GraphEditCommand::SetGraphNodeParam {
                                node_id,
                                param_name: p.name,
                                new_value: crate::SerializedParamValue::Float { value: new_value },
                            });
                        }
                    }
                    // Enum → open a dropdown anchored on this row; picking an
                    // option emits SetGraphNodeParam, or SetOuterParam for a
                    // group-face mirror row (`outer_param_id` rides the
                    // dropdown to the pick handler at the top of this fn).
                    K::Enum => {
                        if !p.enum_labels.is_empty()
                            && let Some(anchor) = self.param_row_rect(viewport, node_id, pi)
                        {
                            let last = p.enum_labels.len() - 1;
                            let cur_idx = (p.current_value.round().max(0.0) as usize).min(last);
                            self.vec_editor = None;
                            self.table_editor = None;
                            self.enum_dropdown = Some(super::EnumDropdown {
                                node_id,
                                param_name: p.name,
                                options: p.enum_labels,
                                current: cur_idx,
                                anchor,
                                outer_param_id: p.outer_param_id,
                            });
                        }
                    }
                    // Color / Vec → open the channel editor anchored on this row;
                    // dragging a channel row emits the SetGraphNodeParam.
                    K::Color | K::Vec2 | K::Vec3 | K::Vec4 => {
                        if let Some(anchor) = self.param_row_rect(viewport, node_id, pi) {
                            self.enum_dropdown = None;
                            self.table_editor = None;
                            self.vec_editor =
                                Some(super::VecEditor::new(node_id, p.name, p.kind, anchor));
                        }
                    }
                    // String → the app's inline text editor for free text
                    // (`EditGraphNodeStringParam`, anchored on the row), or the
                    // native folder picker for a path-like param
                    // (`BrowseGraphNodePath`). Parity with the sidebar's value
                    // cell / Browse button.
                    K::String => {
                        if p.is_path {
                            self.pending_actions.push(GraphEditCommand::BrowseGraphNodePath {
                                node_id,
                                param_name: p.name,
                            });
                        } else if let Some(anchor) = self.param_row_rect(viewport, node_id, pi) {
                            self.pending_actions
                                .push(GraphEditCommand::EditGraphNodeStringParam {
                                    node_id,
                                    param_name: p.name,
                                    current: p.string_value.unwrap_or_default(),
                                    anchor: (anchor.x, anchor.y, anchor.w, anchor.h),
                                });
                        }
                    }
                    _ => {}
                }
                return;
            }
        }
        // "Edit Code…" footer on an expanded `wgsl_compute` node → open the
        // multiline kernel editor (`EditGraphNodeWgsl`; the app re-anchors it over
        // the canvas, so the anchor is unused, matching the sidebar).
        if let Some(node_id) = self.node_under(viewport, sx, sy)
            && let Some(r) = self.wgsl_edit_rect(viewport, node_id)
            && sx >= r.x
            && sx <= r.x + r.w
            && sy >= r.y
            && sy <= r.y + r.h
        {
            let current = self
                .find_node(node_id)
                .and_then(|n| n.wgsl_source.clone())
                .unwrap_or_default();
            self.select_single(node_id);
            self.pending_actions.push(GraphEditCommand::EditGraphNodeWgsl {
                node_id,
                current,
                anchor: (0.0, 0.0, 0.0, 0.0),
            });
            return;
        }
        // "Reveal / hide unused sockets" chip on the node header → flip this
        // node's reveal state and rebuild its rows in place (reveal isn't a
        // topology change, so `set_snapshot`'s hash-gate wouldn't rebuild them).
        // Checked before the header-drag so a click on the chip toggles rather
        // than starting a move.
        if let Some(node_id) = self.node_under(viewport, sx, sy)
            && let Some(r) = self.reveal_chip_rect(viewport, node_id)
            && sx >= r.x
            && sx <= r.x + r.w
            && sy >= r.y
            && sy <= r.y + r.h
        {
            let now_revealed = !self.revealed_ports.get(&node_id).copied().unwrap_or(false);
            self.revealed_ports.insert(node_id, now_revealed);
            self.rebuild_rows();
            self.select_single(node_id);
            return;
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
                self.drag.start(
                    CanvasDrag::NodeMove {
                        node_id,
                        anchor_offset,
                    },
                    crate::node::Vec2::new(sx, sy),
                );
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
                    self.drag.start(CanvasDrag::Marquee, crate::node::Vec2::new(sx, sy));
                } else {
                    // Plain left-drag = pan, so the canvas stays navigable on a
                    // trackpad. A left-click with no drag clears the selection
                    // (handled on release).
                    self.drag.start(
                        CanvasDrag::Pan { pan_at_grab: self.pan },
                        crate::node::Vec2::new(sx, sy),
                    );
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
        // Any click here is a new interaction — a wire highlighted by a
        // previous wire-driven-row click (D5) shouldn't linger past it.
        self.highlighted_wire = None;
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
        // See `click_select`'s note — same "new interaction clears the wire
        // highlight" rule. The wire-driven-row branch that wants to set it
        // calls `select_single` first, then sets `highlighted_wire` after.
        self.highlighted_wire = None;
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
        // Read the grab position before release() consumes the session —
        // Pan and Marquee need it for their release-time geometry.
        let start = self.drag.session().map(|s| s.start);
        let Some(prev) = self.drag.release() else {
            return;
        };
        match prev {
            CanvasDrag::Pan { .. } => {
                // A left-press that didn't actually pan (cursor barely moved) is
                // a click on empty space — clear the selection. A real pan
                // leaves the selection alone.
                let start = start.expect("a Pan session always has a start position");
                let moved = (sx - start.x).hypot(sy - start.y);
                if moved < CLICK_MOVE_SLOP_PX {
                    self.selected.clear();
                }
            }
            CanvasDrag::WireFrom {
                from_node,
                from_port,
            } => {
                // Only commit on drop over an input port — drop on
                // empty or an output cancels silently.
                let valid_drop = self
                    .port_under(viewport, sx, sy)
                    .filter(|hit| !hit.is_output && hit.node_id != from_node);
                match valid_drop {
                    Some(hit) => {
                        // D17 "flow pulse": geometry captured now (screen
                        // space) — `from_port`/`hit.port_name` move into the
                        // command below.
                        if let (Some(from_n), Some(to_n)) =
                            (self.find_node(from_node), self.find_node(hit.node_id))
                            && let Some(fi) = from_n.outputs.iter().position(|p| p.name == from_port)
                            && let Some(ti) = to_n.inputs.iter().position(|p| p.name == hit.port_name)
                        {
                            let (fgx, fgy) = from_n.output_port_pos_graph(fi);
                            let (tgx, tgy) = to_n.input_port_pos_graph(ti);
                            let from_pt = self.to_screen(viewport, fgx, fgy);
                            let to_pt = self.to_screen(viewport, tgx, tgy);
                            self.fire_wire_flow_pulse(from_pt, to_pt);
                        }
                        self.pending_actions.push(GraphEditCommand::ConnectPorts {
                            from_node,
                            from_port,
                            to_node: hit.node_id,
                            to_port: hit.port_name,
                        });
                        // D17 "wire→port ... pop".
                        self.fire_connect_pop(sx, sy);
                    }
                    None => {
                        // D17 error shake — a wire dropped on empty canvas,
                        // an output port, or back onto its own source node.
                        self.fire_error_shake(sx, sy);
                    }
                }
            }
            CanvasDrag::NodeMove { node_id, .. } => {
                if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
                    self.pending_actions.push(GraphEditCommand::MoveGraphNode {
                        node_id,
                        new_pos: node.pos_graph,
                    });
                }
            }
            // The scrub emitted its value on each pointer move; nothing to
            // finalize for an ordinary row. `EndGraphNodeParamScrub` closes out
            // a card-bound row's write-back gesture (PARAM_TWO_WAY_BINDING_
            // DESIGN.md D1) with one undo-worthy commit — a no-op for every
            // unbound row, which the app has nothing tracked for. Only emitted
            // when the press actually moved: a zero-move press+release (a
            // plain click) never emitted a `SetGraphNodeParam` in the first
            // place, so there is nothing to close out — matches the existing
            // "zero-move press+release emits nothing" contract.
            CanvasDrag::ParamScrub { node_id, param_name, .. } => {
                let start = start.expect("a ParamScrub session always has a start position");
                let moved = (sx - start.x).hypot(sy - start.y) >= CLICK_MOVE_SLOP_PX;
                if moved {
                    self.pending_actions.push(GraphEditCommand::EndGraphNodeParamScrub {
                        node_id,
                        param_name,
                    });
                }
            }
            // The Color/Vec editor stays open on release so the next channel
            // can be grabbed — only a press outside it dismisses.
            CanvasDrag::VecScrub { .. } => {}
            CanvasDrag::Marquee => {
                // A shift-press with no real drag leaves the selection alone —
                // don't let a zero-area box wipe it. `origin_screen` is now the
                // session start (D9).
                let start = start.expect("a Marquee session always has a start position");
                let (ox, oy) = (start.x, start.y);
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

    /// Build the `ToggleNodeParamExpose` for a param row's expose glyph, or
    /// `None` when the node has no stable handle (anonymous / boundary nodes
    /// can't be addressed by the exposure command). Captures the inner-param
    /// metadata (label / range / default / convert / enum labels) the outer-card
    /// binding needs — byte-identical to the sidebar's expose path, so an on-node
    /// expose and a sidebar expose produce the same binding.
    fn build_expose_toggle(&self, node_id: u32, pi: usize) -> Option<GraphEditCommand> {
        let node = self.find_node(node_id)?;
        let handle = node.handle.clone()?;
        let p = node.params.get(pi)?;
        let (min, max) = p.range.unwrap_or((0.0, 1.0));
        Some(GraphEditCommand::ToggleNodeParamExpose {
            node_id: node.node_id.clone(),
            // `node_id` (the u32 arg) IS this node's doc id — the reliable key
            // the command locates by, since `node.node_id` (stable) is empty on
            // bundled-preset nodes.
            node_u32_id: node_id,
            node_handle: handle,
            inner_param: p.name.clone(),
            expose: !p.exposed,
            label: p.label.clone(),
            min,
            max,
            default_value: p.default_value,
            convert: param_convert_for_kind(p.kind),
            is_angle: matches!(p.kind, crate::graph_view::ParamSnapshotKind::Angle),
            value_labels: p.enum_labels.clone(),
        })
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
