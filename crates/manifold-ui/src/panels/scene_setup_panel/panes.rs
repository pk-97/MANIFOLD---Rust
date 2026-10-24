//! The Scene tab's two panes (SCENE_SURFACES_DESIGN.md D8 (Scene tab tree)):
//! the tree on top and the selected item's settings below, each its own
//! scroll column, with a split the user drags between them. Blender's
//! Outliner over Properties, Unity's Hierarchy over Inspector: scrolling a
//! long material never carries the tree out of view.
//!
//! The tree pane takes only the height its rows need, up to the split; the
//! split is the tree's cap as a share of the body. The cap and both scroll
//! offsets are UI-local workspace state like fold state, never saved.

use super::*;

const KEY_SPLIT: u64 = 80_027;
/// The grab strip is as tall as the app's other resize handles.
const SPLIT_H: f32 = color::INSPECTOR_RESIZE_HANDLE_WIDTH;
const DEFAULT_SPLIT: f32 = 0.4;
const MIN_SPLIT: f32 = 0.1;
const MAX_SPLIT: f32 = 0.85;
/// The settings pane keeps room for a few rows however far the split goes.
const MIN_SETTINGS_H: f32 = ROW_H * 4.0;
/// The tree pane keeps room for a few rows however small the body gets.
const MIN_TREE_H: f32 = ROW_H * 3.0;

#[derive(Clone, Copy)]
enum Pane {
    Tree,
    Settings,
}

pub(super) struct ScenePanes {
    tree: ScrollContainer,
    split: f32,
    /// `(press offset from the strip's top, moved)` while the split is held.
    drag: Option<(f32, bool)>,
    strip: Rect,
    strip_id: Option<NodeId>,
    tree_top: f32,
    body_h: f32,
    /// The most the tree pane may take before the settings run out of room.
    room: f32,
    /// The tree rows' height at the last build.
    tree_content_h: f32,
    /// This build has two panes; the empty states have one column.
    two: bool,
}

impl ScenePanes {
    fn tree_cap(&self, split: f32) -> f32 {
        (self.body_h * split).min(self.room).max(MIN_TREE_H)
    }

    /// The tree pane's height for `split` with the rows last built.
    fn tree_h(&self, split: f32) -> f32 {
        self.tree_cap(split).min(self.tree_content_h)
    }
}

impl Default for ScenePanes {
    fn default() -> Self {
        Self {
            tree: ScrollContainer::new(),
            split: DEFAULT_SPLIT,
            drag: None,
            strip: Rect::ZERO,
            strip_id: None,
            tree_top: 0.0,
            body_h: 0.0,
            room: 0.0,
            tree_content_h: 0.0,
            two: false,
        }
    }
}

impl ScenePanel {
    /// Everything under the chrome. One scroll column for the empty states;
    /// a live scene gets its header row, the tree pane, the split and the
    /// settings pane.
    pub(super) fn build_body(&mut self, tree: &mut UITree, body: Rect) {
        let inner_x = body.x + PAD;
        let inner_w = body.width - PAD * 2.0;
        let outer = tree.add_node(
            None,
            body,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        );
        // Keeps `under_name: "scene_setup"` selectors addressing both panes.
        tree.set_name(outer, "scene_setup");
        let outer_start = tree.count();
        self.content_parent = outer;
        self.panes.two = false;
        self.panes.strip_id = None;
        let state = self.state.clone();
        let mut settings_top = body.y;
        let mut selected = None;
        if let SceneSetupState::Live(vm) = &state {
            let tree_top = self.build_tree_header(tree, inner_x, inner_w, body.y);
            let sel = self.resolve_selection(vm);
            self.build_tree_pane(tree, body, tree_top, vm, &sel);
            settings_top = self.build_split_strip(tree, outer, body);
            selected = Some(sel);
        }
        let viewport = Rect::new(
            body.x,
            settings_top,
            body.width,
            (body.y + body.height - settings_top).max(0.0),
        );
        let start = self.begin_pane(tree, Pane::Settings, viewport);
        let top = if self.panes.two {
            settings_top + ROW_GAP
        } else {
            settings_top
        };
        let end = match (&state, selected) {
            (SceneSetupState::NoSelection(sentence), _) => {
                self.build_sentence(tree, inner_x, inner_w, top, sentence)
            }
            (SceneSetupState::NoScene { .. }, _) => {
                self.build_no_scene(tree, inner_x, inner_w, top)
            }
            (SceneSetupState::Live(vm), Some(sel)) => {
                self.build_properties(tree, inner_x, inner_w, top, vm, sel)
            }
            (SceneSetupState::Live(_), None) => top,
        };
        self.end_pane(tree, Pane::Settings, start, end - settings_top + PAD);
        if self.reveal_properties
            && let Some(&(_, node, _)) = self.header_buttons.names.first()
        {
            let bounds = tree.get_bounds(node);
            self.scroll.reveal_rect(tree, bounds);
        }
        self.reveal_properties = false;
        tree.reparent_root_nodes(outer_start, tree.count() - outer_start, outer);
    }

    /// The rows above the tree that never scroll. Returns the tree's top.
    fn build_tree_header(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, cy: f32) -> f32 {
        let render_mode = self.render_mode_label();
        let buttons = 2 + usize::from(render_mode.is_some());
        let w = (inner_w - GAP * (buttons - 1) as f32) / buttons as f32;
        let show = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            w,
            ROW_H,
            toggle_btn_style(self.show_gizmos),
            "Show gizmos",
            KEY_SHOW_GIZMOS,
        );
        self.show_gizmos_id = Some(show);
        let mode = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + (w + GAP),
            cy,
            w,
            ROW_H,
            btn_style(),
            &format!("Gizmos: {}", self.gizmo_draw_mode.label()),
            KEY_GIZMO_MODE,
        );
        tree.set_name(mode, "scene_setup.gizmo_mode");
        self.gizmo_mode_id = Some(mode);
        if let Some(label) = render_mode {
            let render = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + (w + GAP) * 2.0,
                cy,
                w,
                ROW_H,
                btn_style(),
                &label,
                KEY_RENDER_MODE,
            );
            tree.set_name(render, "scene_setup.render_mode");
            self.render_mode_id = Some(render);
        }
        let mut next_y = cy + ROW_H + ROW_GAP;
        if self.preview_navigating {
            let reset = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x,
                next_y,
                inner_w,
                ROW_H,
                toggle_btn_style(true),
                "Back to Camera (Esc)",
                crate::param_surface::stable_key("scene.reset_preview_view"),
            );
            tree.set_name(reset, "scene_setup.reset_preview_view");
            self.reset_view_id = Some(reset);
            next_y += ROW_H + ROW_GAP;
        }
        next_y
    }

    /// The Render Mode row's pick list, anchored under its header button: the
    /// scene's own manifest param, opened through the shared pick-list action.
    pub(super) fn render_mode_pick_list(&self, button: NodeId) -> Option<PanelAction> {
        let row = self.render_mode_row()?;
        let target = GraphParamTarget::GeneratorOf(self.live_layer_id()?.clone());
        crate::panels::param_slider_shared::pick_list_action(
            target,
            row.id.clone(),
            &row.spec,
            row.value.base,
            button,
        )
    }

    fn build_tree_pane(
        &mut self,
        tree: &mut UITree,
        body: Rect,
        top: f32,
        vm: &SceneSetupVm,
        selected: &SceneSelection,
    ) {
        self.panes.room = body.y + body.height - top - SPLIT_H - MIN_SETTINGS_H;
        self.panes.tree_top = top;
        self.panes.body_h = body.height;
        self.panes.two = true;
        let cap = self.panes.tree_cap(self.panes.split);
        let start = self.begin_pane(tree, Pane::Tree, Rect::new(body.x, top, body.width, cap));
        // Every selected row highlights; the active one's card shows below.
        let highlighted = self.selected_of(&vm.layer_id).to_vec();
        let end = self.build_outliner(
            tree,
            body.x + PAD,
            body.width - PAD * 2.0,
            top,
            vm,
            &highlighted,
        );
        let content_h = end - top + ROW_GAP;
        self.panes.tree_content_h = content_h;
        self.panes
            .tree
            .set_viewport_height(tree, content_h.min(cap));
        self.end_pane(tree, Pane::Tree, start, content_h);
        // An outside pick (a viewport click) scrolls its row into view.
        if self.reveal_properties
            && let Some(&(row, _)) = self
                .outliner_row_ids
                .iter()
                .find(|(_, sel)| sel == selected)
        {
            let bounds = tree.get_bounds(row);
            self.panes.tree.reveal_rect(tree, bounds);
        }
    }

    /// The grab strip under the tree pane, drawn as a hairline that lights
    /// up on hover like the app's other split handles. Returns the settings
    /// pane's top.
    fn build_split_strip(&mut self, tree: &mut UITree, outer: NodeId, body: Rect) -> f32 {
        let tree_vp = self.panes.tree.viewport();
        let y = tree_vp.y + tree_vp.height;
        self.panes.strip = Rect::new(body.x, y, body.width, SPLIT_H);
        let line = UIStyle {
            bg_color: color::MODAL_BORDER,
            ..UIStyle::default()
        };
        tree.add_panel(
            Some(outer),
            body.x + PAD,
            y + (SPLIT_H - 1.0) * 0.5,
            body.width - PAD * 2.0,
            1.0,
            line,
        );
        let held = self.panes.drag.is_some_and(|(_, moved)| moved);
        let style = UIStyle {
            bg_color: if held {
                color::RESIZE_HANDLE_DRAG
            } else {
                color::RESIZE_HANDLE_IDLE
            },
            hover_bg_color: color::RESIZE_HANDLE_HOVER,
            pressed_bg_color: color::RESIZE_HANDLE_DRAG,
            ..UIStyle::default()
        };
        let id = tree.add_button_keyed(
            Some(outer),
            body.x,
            y,
            body.width,
            SPLIT_H,
            style,
            "",
            KEY_SPLIT,
        );
        tree.set_name(id, "scene_setup.split");
        self.panes.strip_id = Some(id);
        y + SPLIT_H
    }

    fn pane(&mut self, pane: Pane) -> &mut ScrollContainer {
        match pane {
            Pane::Tree => &mut self.panes.tree,
            Pane::Settings => &mut self.scroll,
        }
    }

    fn begin_pane(&mut self, tree: &mut UITree, pane: Pane, viewport: Rect) -> usize {
        let clip = self.pane(pane).begin(tree, viewport);
        tree.set_name(
            clip,
            match pane {
                Pane::Tree => "scene_setup.tree_pane",
                Pane::Settings => "scene_setup.settings_pane",
            },
        );
        self.content_parent = clip;
        tree.count()
    }

    fn end_pane(&mut self, tree: &mut UITree, pane: Pane, start: usize, content_h: f32) {
        let sb_x = self.panel_rect.x + self.panel_w - SCROLLBAR_W - 2.0;
        let scroll = self.pane(pane);
        scroll.set_content_height(content_h.max(0.0));
        scroll.reparent_content(tree, start);
        let offset = scroll.scroll_offset();
        if offset != 0.0 {
            scroll.offset_content(tree, -offset);
        }
        scroll.build_scrollbar(tree, sb_x, &scrollbar_style());
    }

    /// The split's press, drag, release and double-click reset, and the
    /// mouse wheel routed to the pane under the pointer. `None` leaves the
    /// event to the rest of the panel.
    pub(super) fn handle_pane_event(
        &mut self,
        event: &UIEvent,
    ) -> Option<(bool, Vec<PanelAction>)> {
        let strip = self.panes.strip_id;
        let on_strip = |node: NodeId| strip == Some(node);
        match event {
            UIEvent::PointerDown { node_id, pos, .. } if on_strip(*node_id) => {
                self.panes.drag = Some((pos.y - self.panes.strip.y, false));
            }
            UIEvent::Click { node_id, .. } if on_strip(*node_id) => {}
            UIEvent::DoubleClick { node_id, .. } if on_strip(*node_id) => {
                self.set_split(DEFAULT_SPLIT);
            }
            UIEvent::DragBegin { .. } if self.panes.drag.is_some() => {}
            UIEvent::Drag { pos, .. } if self.panes.drag.is_some() => self.drag_split_to(pos.y),
            UIEvent::DragEnd { pos, .. } | UIEvent::PointerUp { pos, .. }
                if self.panes.drag.is_some() =>
            {
                // The split lands where the button came up, and the strip
                // drops its held colour.
                if self.panes.drag.is_some_and(|(_, moved)| moved) {
                    self.drag_split_to(pos.y);
                    self.view_version += 1;
                }
                self.panes.drag = None;
            }
            UIEvent::Scroll { pos, delta, .. } => {
                self.handle_scroll_at(*pos, delta.y);
            }
            _ => return None,
        }
        Some((true, Vec::new()))
    }

    fn drag_split_to(&mut self, y: f32) {
        let Some((grab, moved)) = self.panes.drag else {
            return;
        };
        self.panes.drag = Some((grab, true));
        if !moved {
            // The first move repaints the strip in its held colour.
            self.view_version += 1;
        }
        if self.panes.body_h > 0.0 {
            self.set_split((y - grab - self.panes.tree_top) / self.panes.body_h);
        }
    }

    /// A rebuild only when the tree pane's height changes (UI_INVALIDATION
    /// D19): a split held at its clamp, or past a short tree's last row,
    /// draws the same panes.
    fn set_split(&mut self, split: f32) {
        let split = split.clamp(MIN_SPLIT, MAX_SPLIT);
        let before = self.panes.tree_h(self.panes.split);
        self.panes.split = split;
        if self.panes.tree_h(split) != before {
            self.view_version += 1;
        }
    }

    /// Mouse-wheel scroll for the settings pane.
    pub fn handle_scroll(&mut self, delta: f32) -> bool {
        let moved = self.scroll.apply_scroll_delta(delta);
        if moved {
            self.view_version += 1;
        }
        moved
    }

    /// Mouse-wheel scroll for whichever pane is under `pos`.
    pub fn handle_scroll_at(&mut self, pos: Vec2, delta: f32) -> bool {
        if !(self.panes.two && self.panes.tree.viewport().contains(pos)) {
            return self.handle_scroll(delta);
        }
        let moved = self.panes.tree.apply_scroll_delta(delta);
        if moved {
            self.view_version += 1;
        }
        moved
    }

    /// The viewport of the pane `node` scrolls in: the tree pane for a tree
    /// row, the settings pane for anything else.
    pub fn scroll_viewport_of(&self, tree: &UITree, node: Option<NodeId>) -> Rect {
        let tree_clip = self.panes.tree.clip_node_id().filter(|_| self.panes.two);
        let mut cur = node;
        while let Some(id) = cur {
            if Some(id) == tree_clip {
                return self.panes.tree.viewport();
            }
            cur = tree.parent_of(id);
        }
        self.scroll.viewport()
    }

    /// Whether the pointer should show the vertical resize cursor: over the
    /// split, or holding it. `shown` is the Scene tab's on-screen rect
    /// (`InspectorCompositePanel::scene_content_rect`, zero while the tab or
    /// the inspector is hidden): the panel stays open on a zero-width
    /// inspector and keeps its last strip rect, which must not reach the
    /// cursor over whatever is drawn there now.
    pub fn split_cursor(&self, pos: Vec2, shown: Rect) -> bool {
        let visible = self.open && self.panes.two && shown.width > 0.0 && shown.height > 0.0;
        visible
            && (self.panes.drag.is_some()
                || (self.panes.strip.contains(pos) && shown.contains(pos)))
    }
}
