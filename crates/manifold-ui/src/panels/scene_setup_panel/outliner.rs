//! The Scene tab tree (SCENE_DOCUMENT_DESIGN.md section 8): the item
//! hierarchy as it is in the document. World and the cameras are pinned at
//! the top, then one Lighting row for every light, then every other item in
//! its place with a kind icon, nested groups shown as a real tree, then the
//! Forces and the Modifiers chain in their own sections. Each row
//! is the item's kind icon, its name, and an eye where the item has Visible.

use super::*;
use crate::icons::scene_item_icon;

fn eye_glyph(value: f32) -> &'static str {
    if value > 0.5 { "\u{1F441}" } else { "\u{2013}" }
}

const KEY_LIGHTING: u64 = 90_000_002;

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

/// An item's Visible param id, when the kind has one.
fn visible_param(item: &SceneItemVm) -> Option<String> {
    let id = format!("{}.visible", item.id);
    item.parameter_ids.contains(&id).then_some(id)
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
        let add = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            STEP_W * 3.0,
            ROW_H,
            btn_style(),
            "+ Add",
            KEY_ADD,
        );
        tree.set_name(add, "scene_tree.add");
        self.add_id = Some(add);
        cy += ROW_H;

        let pinned = |item: &SceneItemVm| item.depth == 0 && matches!(item.kind, ItemKind::World | ItemKind::Camera);
        let ordered = vm
            .items
            .iter()
            .filter(|item| item.depth == 0 && item.kind == ItemKind::World)
            .chain(vm.items.iter().filter(|item| item.depth == 0 && item.kind == ItemKind::Camera));
        for item in ordered {
            cy = self.build_item_row(tree, (inner_x, inner_w, 0.0), cy, vm, item, selected);
        }
        if vm.items.iter().any(|item| item.kind == ItemKind::Light) {
            cy = self.build_lighting_row(tree, (inner_x, inner_w), cy, selected);
        }
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
            if pinned(item) || matches!(item.kind, ItemKind::Light | ItemKind::Force | ItemKind::Modifier) {
                continue;
            }
            let indent = item.depth as f32 * STEP_W;
            cy = self.build_item_row(tree, (inner_x, inner_w, indent), cy, vm, item, selected);
        }
        // Forces and Modifiers act on the scene, so they sit apart from it,
        // each in its own section. Modifiers are listed in the order they
        // apply; dragging a card reorders the chain.
        for (title, kind) in [("Forces", ItemKind::Force), ("Modifiers", ItemKind::Modifier)] {
            cy += ROW_GAP;
            let header = tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, title, label_style());
            tree.set_name(header, format!("scene_tree.section.{}", title.to_lowercase()));
            cy += ROW_H;
            for item in vm.items.iter().filter(|item| item.kind == kind) {
                cy = self.build_item_row(tree, (inner_x, inner_w, 0.0), cy, vm, item, selected);
                if kind == ItemKind::Modifier
                    && let Some((row, _)) = self.outliner_row_ids.last()
                {
                    self.chain_rows.push((*row, item.id.clone()));
                }
            }
        }
        cy
    }

    fn build_lighting_row(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), cy: f32, selected: &[SceneSelection]) -> f32 {
        let is_selected = selected.contains(&SceneSelection::Lighting);
        tree.add_label(Some(self.content_parent), inner_x, cy, STEP_W, ROW_H, &scene_item_icon(ItemKind::Light, false).text(), icon_style(is_selected));
        let row = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + STEP_W,
            cy,
            (inner_w - STEP_W * 2.0).max(0.0),
            ROW_H,
            outliner_row_style(is_selected),
            "Lighting",
            KEY_LIGHTING,
        );
        tree.set_name(row, "scene_tree.item");
        self.outliner_row_ids.push((row, SceneSelection::Lighting));
        cy + ROW_H
    }

    /// One selectable row: `[chevron | icon | name | eye]`, indented by
    /// `indent`. A group with items inside carries a chevron that folds them
    /// and never changes selection.
    fn build_item_row(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w, indent): (f32, f32, f32),
        cy: f32,
        vm: &SceneSetupVm,
        item: &SceneItemVm,
        selected: &[SceneSelection],
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
        let eye_w = if eye.is_some() { STEP_W } else { 0.0 };
        let row = tree.add_button_keyed(
            Some(self.content_parent),
            x + STEP_W,
            cy,
            (inner_w - (x - inner_x) - STEP_W - eye_w).max(0.0),
            ROW_H,
            outliner_row_style(is_selected),
            &item.name,
            outliner_row_key(&selection),
        );
        tree.set_name(row, "scene_tree.item");
        self.outliner_row_ids.push((row, selection));
        if let Some(param) = eye {
            let value = self.visible_value(&param);
            let eye_id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                btn_style(),
                eye_glyph(value),
                outliner_eye_key(&item.id),
            );
            tree.set_name(eye_id, format!("scene_setup.object_eye.{}", item.id));
            self.outliner_eye_ids.push((eye_id, param, value));
        }
        cy + ROW_H
    }
}
