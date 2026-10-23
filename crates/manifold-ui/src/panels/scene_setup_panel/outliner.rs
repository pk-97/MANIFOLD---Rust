//! The Scene tab tree (SCENE_DOCUMENT_DESIGN.md section 8, amended by
//! SCENE_REFINEMENT_DESIGN.md ruling 4): World, then the cameras, one
//! Lighting row for every light, then the Objects, the Forces and the
//! Modifiers chain, each in its own section. The Camera row, the Lighting
//! row and the three section headers each carry a + that adds what belongs
//! there. Each row is the item's kind icon, its name, and an eye where the
//! item has Visible. A Material is a row only when two or more items use it
//! (ruling 1); a Body inside a Group is marked.

use super::*;
use crate::icons::scene_item_icon;

fn eye_glyph(value: f32) -> &'static str {
    if value > 0.5 { "\u{1F441}" } else { "\u{2013}" }
}

const KEY_LIGHTING: u64 = 90_000_002;
const BODY_MARK: &str = "Body";

/// A place in the tree that carries a +, and the Add menu sections it offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TreeSection {
    Camera,
    Lighting,
    Objects,
    Forces,
    Modifiers,
}

impl TreeSection {
    fn title(self) -> &'static str {
        match self {
            TreeSection::Camera => "Camera",
            TreeSection::Lighting => "Lighting",
            TreeSection::Objects => "Objects",
            TreeSection::Forces => "Forces",
            TreeSection::Modifiers => "Modifiers",
        }
    }

    /// The Add menu sections (`AddEntryVm::group`) its + offers, in menu
    /// order. Water sources are offered only while a Water is selected,
    /// since they sit inside one.
    pub(super) fn groups(self) -> &'static [&'static str] {
        match self {
            TreeSection::Camera => &["Camera"],
            TreeSection::Lighting => &["Light"],
            TreeSection::Objects => &["Object", "Group", "Swarm", "Water", "Water Source"],
            TreeSection::Forces => &["Force"],
            TreeSection::Modifiers => &["Modifier"],
        }
    }

    fn key(self) -> u64 {
        crate::param_surface::stable_key(&format!("scene.add.{}", self.title()))
    }
}

/// Stable row key from the selection identity.
fn outliner_row_key(sel: &SceneSelection) -> u64 {
    match sel {
        SceneSelection::Lighting => KEY_LIGHTING,
        SceneSelection::Item(id) => crate::param_surface::stable_key(&format!("scene.item.{id}")),
    }
}

fn outliner_eye_key(item: &ItemId) -> u64 {
    crate::param_surface::stable_key(&format!("scene.eye.{item}"))
}

fn outliner_group_toggle_key(group: &ItemId) -> u64 {
    crate::param_surface::stable_key(&format!("scene.group_toggle.{group}"))
}

/// Selected rows take a low-alpha wash of the app-wide selection colour,
/// never a border box (the `layer_header.rs` precedent).
fn outliner_row_style(selected: bool) -> UIStyle {
    let ring = color::SELECTED_LAYER_RING;
    let sel_bg = Color32::new(ring.r, ring.g, ring.b, 40);
    let sel_hover = Color32::new(ring.r, ring.g, ring.b, 60);
    UIStyle {
        bg_color: if selected { sel_bg } else { Color32::TRANSPARENT },
        hover_bg_color: if selected { sel_hover } else { Color32::new(255, 255, 255, 18) },
        text_color: if selected { ring } else { Color32::new(200, 200, 208, 255) },
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}

fn icon_style(selected: bool) -> UIStyle {
    UIStyle {
        text_color: if selected { color::SELECTED_LAYER_RING } else { color::TEXT_DIMMED_C32 },
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

fn mark_style() -> UIStyle {
    UIStyle { text_color: color::TEXT_DIMMED_C32, font_size: color::FONT_CAPTION, text_align: TextAlign::Right, ..UIStyle::default() }
}

/// An item's Visible param id, when the kind has one.
fn visible_param(item: &SceneItemVm) -> Option<String> {
    let id = format!("{}.visible", item.id);
    item.parameter_ids.contains(&id).then_some(id)
}

/// Whether `item` has a row of its own in the tree: a Material only when two
/// or more items use it; it shows inline on its one user's card otherwise.
fn has_tree_row(item: &SceneItemVm) -> bool {
    item.kind != ItemKind::Material || item.material.as_ref().is_some_and(|material| material.used_by >= 2)
}

impl ScenePanel {
    /// Where the eye of `item` reads its value: the layer's full surface.
    fn visible_value(&self, param: &str) -> f32 {
        self.full_params
            .as_ref()
            .and_then(|surface| self.full_param_id_index.get(param).and_then(|&i| surface.rows.get(i)))
            .map_or(1.0, |row| row.value.base)
    }

    pub(super) fn build_outliner(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: &[SceneSelection],
    ) -> f32 {
        let rows = (inner_x, inner_w, 0.0);
        for item in vm.items.iter().filter(|item| item.depth == 0 && item.kind == ItemKind::World) {
            cy = self.build_item_row(tree, rows, cy, vm, item, selected, None);
        }
        // The Camera row's + sits on the first camera; with none, on a bare
        // Camera row.
        let mut cameras = vm.items.iter().filter(|item| item.depth == 0 && item.kind == ItemKind::Camera).peekable();
        if cameras.peek().is_none() {
            cy = self.build_bare_row(tree, (inner_x, inner_w), cy, ItemKind::Camera, TreeSection::Camera);
        }
        for (index, item) in cameras.enumerate() {
            cy = self.build_item_row(tree, rows, cy, vm, item, selected, (index == 0).then_some(TreeSection::Camera));
        }
        cy = if vm.items.iter().any(|item| item.kind == ItemKind::Light) {
            self.build_lighting_row(tree, (inner_x, inner_w), cy, selected)
        } else {
            self.build_bare_row(tree, (inner_x, inner_w), cy, ItemKind::Light, TreeSection::Lighting)
        };

        cy = self.build_section_header(tree, (inner_x, inner_w), cy, TreeSection::Objects);
        let collapsed = self.collapsed_groups.get(&vm.layer_id).cloned().unwrap_or_default();
        // The depth at which a folded group hides what is under it.
        let mut hidden_below: Option<usize> = None;
        for item in &vm.items {
            if let Some(depth) = hidden_below {
                if item.depth > depth {
                    continue;
                }
                hidden_below = None;
            }
            if item.kind == ItemKind::Group && item.has_children && collapsed.contains(&item.id) {
                hidden_below = Some(item.depth);
            }
            let pinned = item.depth == 0 && matches!(item.kind, ItemKind::World | ItemKind::Camera);
            if pinned || matches!(item.kind, ItemKind::Light | ItemKind::Force | ItemKind::Modifier) || !has_tree_row(item) {
                continue;
            }
            let indent = item.depth as f32 * STEP_W;
            cy = self.build_item_row(tree, (inner_x, inner_w, indent), cy, vm, item, selected, None);
        }
        // Forces and Modifiers act on the scene, so they sit apart from it.
        // Modifiers are listed in the order they apply; dragging a card
        // reorders the chain.
        for (section, kind) in [(TreeSection::Forces, ItemKind::Force), (TreeSection::Modifiers, ItemKind::Modifier)] {
            cy = self.build_section_header(tree, (inner_x, inner_w), cy, section);
            for item in vm.items.iter().filter(|item| item.kind == kind) {
                cy = self.build_item_row(tree, rows, cy, vm, item, selected, None);
                if kind == ItemKind::Modifier
                    && let Some((row, _)) = self.outliner_row_ids.last()
                {
                    self.chain_rows.push((*row, item.id.clone()));
                }
            }
        }
        self.build_chain_drop_line(tree, inner_x, inner_w);
        cy
    }

    /// A section title with its + at the right end.
    fn build_section_header(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), mut cy: f32, section: TreeSection) -> f32 {
        cy += ROW_GAP;
        let title = section.title();
        let header = tree.add_label(Some(self.content_parent), inner_x, cy, (inner_w - STEP_W).max(0.0), ROW_H, title, label_style());
        tree.set_name(header, format!("scene_tree.section.{}", title.to_lowercase()));
        self.build_add_button(tree, inner_x + inner_w - STEP_W, cy, section);
        cy + ROW_H
    }

    fn build_add_button(&mut self, tree: &mut UITree, x: f32, cy: f32, section: TreeSection) {
        let add = tree.add_button_keyed(Some(self.content_parent), x, cy, STEP_W, ROW_H, btn_style(), "+", section.key());
        tree.set_name(add, format!("scene_tree.add.{}", section.title()));
        if let Some(node) = tree.get_node_mut(add) {
            node.tooltip = Some(format!("Add to {}", section.title()));
        }
        self.add_buttons.push((add, section));
    }

    /// A Camera or Lighting row with nothing in it yet: its icon, its name
    /// dimmed, and its +.
    fn build_bare_row(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), cy: f32, kind: ItemKind, section: TreeSection) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x + STEP_W, cy, STEP_W, ROW_H, &scene_item_icon(kind, false).text(), icon_style(false));
        let label = tree.add_label(
            Some(self.content_parent),
            inner_x + STEP_W * 2.0 + 4.0,
            cy,
            (inner_w - STEP_W * 3.0).max(0.0),
            ROW_H,
            section.title(),
            label_style(),
        );
        tree.set_name(label, "scene_tree.empty_section");
        self.build_add_button(tree, inner_x + inner_w - STEP_W, cy, section);
        cy + ROW_H
    }

    fn build_lighting_row(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), cy: f32, selected: &[SceneSelection]) -> f32 {
        let is_selected = selected.contains(&SceneSelection::Lighting);
        // The chevron column stays empty, so Lighting lines up with every
        // other top-level row.
        let x = inner_x + STEP_W;
        tree.add_label(Some(self.content_parent), x, cy, STEP_W, ROW_H, &scene_item_icon(ItemKind::Light, false).text(), icon_style(is_selected));
        let row = tree.add_button_keyed(
            Some(self.content_parent),
            x + STEP_W,
            cy,
            (inner_w - STEP_W * 3.0).max(0.0),
            ROW_H,
            outliner_row_style(is_selected),
            "Lighting",
            KEY_LIGHTING,
        );
        tree.set_name(row, "scene_tree.item");
        self.outliner_row_ids.push((row, SceneSelection::Lighting));
        self.build_add_button(tree, inner_x + inner_w - STEP_W, cy, TreeSection::Lighting);
        cy + ROW_H
    }

    /// One selectable row: `[chevron | icon | name | mark | eye | +]`,
    /// indented by `indent`. A group with items inside carries a chevron that
    /// folds them and never changes selection. `add` puts a section's + at
    /// the row's end.
    #[allow(clippy::too_many_arguments)]
    fn build_item_row(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w, indent): (f32, f32, f32),
        cy: f32,
        vm: &SceneSetupVm,
        item: &SceneItemVm,
        selected: &[SceneSelection],
        add: Option<TreeSection>,
    ) -> f32 {
        let selection = SceneSelection::Item(item.id.clone());
        let is_selected = selected.contains(&selection);
        let mut x = inner_x + indent;
        if item.kind == ItemKind::Group && item.has_children {
            let open = !self.collapsed_groups.get(&vm.layer_id).is_some_and(|collapsed| collapsed.contains(&item.id));
            let toggle = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                cy,
                STEP_W,
                ROW_H,
                btn_style(),
                if open { "\u{25BE}" } else { "\u{25B8}" },
                outliner_group_toggle_key(&item.id),
            );
            tree.set_name(toggle, "scene_setup.objects.group_toggle");
            self.group_toggle_ids.push((toggle, item.id.clone()));
        }
        x += STEP_W;
        let icon = tree.add_label(
            Some(self.content_parent),
            x,
            cy,
            STEP_W,
            ROW_H,
            &scene_item_icon(item.kind, item.plane).text(),
            icon_style(is_selected),
        );
        tree.set_name(icon, "scene_tree.icon");
        let eye = visible_param(item);
        let add_w = if add.is_some() { STEP_W } else { 0.0 };
        let eye_w = if eye.is_some() { STEP_W } else { 0.0 };
        // A Body inside a Group: the group's own Physics row reads off while
        // this part falls, so the part says what it is.
        let mark_w = if item.body && item.depth > 0 {
            tree.text_width(BODY_MARK, color::FONT_CAPTION, crate::node::FontWeight::Regular) + 8.0
        } else {
            0.0
        };
        let right = inner_x + inner_w - add_w - eye_w;
        let row = tree.add_button_keyed(
            Some(self.content_parent),
            x + STEP_W,
            cy,
            (right - (x + STEP_W)).max(0.0),
            ROW_H,
            outliner_row_style(is_selected),
            &item.name,
            outliner_row_key(&selection),
        );
        tree.set_name(row, "scene_tree.item");
        self.outliner_row_ids.push((row, selection));
        if mark_w > 0.0 {
            let mark = tree.add_label(Some(self.content_parent), right - mark_w, cy, mark_w - 4.0, ROW_H, BODY_MARK, mark_style());
            tree.set_name(mark, "scene_tree.body_mark");
        }
        if let Some(param) = eye {
            let value = self.visible_value(&param);
            let eye_id = tree.add_button_keyed(Some(self.content_parent), right, cy, STEP_W, ROW_H, btn_style(), eye_glyph(value), outliner_eye_key(&item.id));
            tree.set_name(eye_id, format!("scene_setup.object_eye.{}", item.id));
            self.outliner_eye_index.insert(param.clone(), self.outliner_eye_ids.len());
            self.outliner_eye_ids.push((eye_id, param, value));
        }
        if let Some(section) = add {
            self.build_add_button(tree, inner_x + inner_w - STEP_W, cy, section);
        }
        cy + ROW_H
    }

    /// While a Modifier card is dragged, a line where it will land: above
    /// the card it would go before, or under the last card.
    fn build_chain_drop_line(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32) {
        let (Some(dragged), Some(to)) = (self.chain_drag.as_ref(), self.chain_drop_at) else { return };
        let others: Vec<Rect> = self.chain_rows.iter().filter(|(_, card)| card != dragged).map(|(row, _)| tree.get_bounds(*row)).collect();
        let y = match (others.get(to), others.last()) {
            (Some(row), _) => row.y,
            (None, Some(last)) => last.y + last.height,
            (None, None) => return,
        };
        let style = UIStyle { bg_color: color::SELECTED_LAYER_RING, corner_radius: 1.0, ..UIStyle::default() };
        let line = tree.add_panel(Some(self.content_parent), inner_x + STEP_W, y - 1.0, (inner_w - STEP_W).max(0.0), 2.0, style);
        tree.set_name(line, "scene_tree.drop_line");
    }
}
