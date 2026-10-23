//! The selected item's header (SCENE_SURFACES D8): its name once (click to
//! rename), its kind, and Frame, copy and delete where they apply. Each
//! light's header heads its sub-section inside Lighting. The Camera header
//! also carries "Use this view" (D14); the World of a layer that stopped
//! drawing carries Draw Again, and a Scene Loop with no Loop camera carries
//! Add Loop Camera. A Modifier's header carries its Type and its on/off
//! switch in place of the kind (SCENE_DOCUMENT_DESIGN.md section 8); a Swarm
//! drawn as a scene item links to that item as its look.

use super::*;
use crate::panels::param_slider_shared::{TOGGLE_BTN_H, TOGGLE_BTN_W, dropdown_trigger_style, field_row_action, switch_text};

const USE_VIEW: &str = "Use this view";

/// Header buttons built this frame, cleared on every rebuild.
#[derive(Default)]
pub(super) struct HeaderButtons {
    /// `(item, name button, current name)`: a click opens the rename box.
    pub(super) names: Vec<(ItemId, NodeId, String)>,
    remove: Vec<(NodeId, ItemId)>,
    duplicate: Vec<(NodeId, ItemId)>,
    frame: Vec<(NodeId, ItemId)>,
    use_view: Option<(NodeId, ItemId)>,
    aim_sun: Option<(NodeId, String)>,
    draw_again: Option<NodeId>,
    add_loop_camera: Option<NodeId>,
    /// A Modifier's Type and On: each control and the field row it sets.
    fields: Vec<(NodeId, ParamRow)>,
    /// A Swarm's look link and the item it selects.
    pub(super) look: Option<(NodeId, ItemId)>,
}

impl HeaderButtons {
    pub(super) fn clear(&mut self) {
        self.names.clear();
        self.remove.clear();
        self.duplicate.clear();
        self.frame.clear();
        self.use_view = None;
        self.aim_sun = None;
        self.draw_again = None;
        self.add_loop_camera = None;
        self.fields.clear();
        self.look = None;
    }
}

fn kind_style() -> UIStyle {
    UIStyle {
        text_color: color::TEXT_DIMMED_C32,
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Right,
        ..UIStyle::default()
    }
}

fn item_key(item: &ItemId, what: &str) -> u64 {
    crate::param_surface::stable_key(&format!("scene.header.{what}.{item}"))
}

impl ScenePanel {
    /// The selected item's header: name and kind, then the buttons its kind
    /// takes. Returns the next row's top.
    pub(super) fn build_item_header(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), cy: f32, item: &SceneItemVm) -> f32 {
        // Frame needs two icon-cell widths for its label.
        let frame_w = STEP_W * 2.0;
        let framed = can_frame(item.kind);
        let view_w = if item.copy_view {
            tree.text_width(USE_VIEW, color::FONT_LABEL, crate::node::FontWeight::Regular) + 16.0
        } else {
            0.0
        };
        let header_fields: Vec<(&FieldRowVm, f32)> = item
            .fields
            .iter()
            .filter(|field| field.header)
            .map(|field| {
                let width = match &field.control {
                    FieldControlVm::Switch { .. } => TOGGLE_BTN_W,
                    FieldControlVm::Pick { current, .. } => {
                        tree.text_width(current, color::FONT_LABEL, crate::node::FontWeight::Regular) + 24.0
                    }
                    FieldControlVm::File { .. } => 0.0,
                };
                (field, width)
            })
            .collect();
        let fields_w: f32 = header_fields.iter().map(|(_, width)| width + 4.0).sum();
        let buttons_w = STEP_W * (2.0 * f32::from(u8::from(item.removable)))
            + if framed { frame_w } else { 0.0 }
            + view_w
            + fields_w;
        let name_w = (inner_w - buttons_w - if buttons_w > 0.0 { 4.0 } else { 0.0 }).max(0.0);
        self.build_name_and_kind(tree, Rect::new(inner_x, cy, name_w, ROW_H), item, header_fields.is_empty());
        let mut x = inner_x + name_w + 4.0;
        for (field, width) in header_fields {
            let row = super::properties::field_param_row(&item.id, field);
            let (text, mut style, y, h) = match &field.control {
                FieldControlVm::Switch { on, .. } => {
                    (switch_text(*on).to_owned(), toggle_btn_style(*on), cy + (ROW_H - TOGGLE_BTN_H) * 0.5, TOGGLE_BTN_H)
                }
                FieldControlVm::Pick { current, .. } => (current.clone(), dropdown_trigger_style(color::FONT_LABEL), cy, ROW_H),
                FieldControlVm::File { .. } => continue,
            };
            if field.disabled.is_some() {
                style.text_color = color::TEXT_DIMMED_C32;
            }
            let id = tree.add_button_keyed(Some(self.content_parent), x, y, width, h, style, &text, item_key(&item.id, &field.id));
            tree.set_name(id, format!("scene_setup.properties.header_field.{}", field.id));
            if let Some(node) = tree.get_node_mut(id) {
                node.tooltip.clone_from(&field.disabled);
            }
            self.header_buttons.fields.push((id, row));
            x += width + 4.0;
        }
        if item.copy_view {
            let id = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                cy,
                view_w,
                ROW_H,
                btn_style(),
                USE_VIEW,
                item_key(&item.id, "use_view"),
            );
            tree.set_name(id, "scene_setup.properties.use_view");
            self.header_buttons.use_view = Some((id, item.id.clone()));
            x += view_w;
        }
        if framed {
            let id = tree.add_button_keyed(Some(self.content_parent), x, cy, frame_w, ROW_H, btn_style(), "Frame", item_key(&item.id, "frame"));
            tree.set_name(id, "scene_setup.properties.frame");
            self.header_buttons.frame.push((id, item.id.clone()));
            x += frame_w;
        }
        if item.removable {
            let dup = tree.add_button_keyed(Some(self.content_parent), x, cy, STEP_W, ROW_H, btn_style(), "\u{29C9}", item_key(&item.id, "dup"));
            tree.set_name(dup, "scene_setup.properties.duplicate");
            self.header_buttons.duplicate.push((dup, item.id.clone()));
            let remove =
                tree.add_button_keyed(Some(self.content_parent), x + STEP_W, cy, STEP_W, ROW_H, btn_style(), "\u{2715}", item_key(&item.id, "remove"));
            tree.set_name(remove, "scene_setup.properties.remove");
            self.header_buttons.remove.push((remove, item.id.clone()));
        }
        let mut cy = cy + ROW_H + ROW_GAP;
        if let Some((look, name)) = &item.look {
            let id = self.action_button(tree, (inner_x, inner_w), cy, item, &format!("Look: {name}"), "scene_setup.properties.look");
            if let Some(node) = tree.get_node_mut(id) {
                node.tooltip = Some(format!("Select {name}, the item this swarm draws"));
            }
            self.header_buttons.look = Some((id, look.clone()));
            cy += ROW_H + ROW_GAP;
        }
        if item.draw_again {
            self.header_buttons.draw_again = Some(self.action_button(tree, (inner_x, inner_w), cy, item, "Draw Again", "scene_setup.properties.draw_again"));
            cy += ROW_H + ROW_GAP;
        }
        if item.add_loop_camera {
            self.header_buttons.add_loop_camera = Some(self.action_button(tree, (inner_x, inner_w), cy, item, "Add Loop Camera", "scene_setup.properties.add_loop_camera"));
            cy += ROW_H + ROW_GAP;
        }
        cy
    }

    /// A full-width button under the header that acts on the whole item.
    fn action_button(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), cy: f32, item: &SceneItemVm, label: &str, name: &'static str) -> NodeId {
        let id = tree.add_button_keyed(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, btn_style(), label, item_key(&item.id, name));
        tree.set_name(id, name);
        id
    }

    /// One light's header inside Lighting: its name (click to rename) and
    /// kind, copy and delete, then "Aim Sun at HDRI" for an HDRI environment.
    pub(super) fn build_light_header(&mut self, tree: &mut UITree, (inner_x, inner_w): (f32, f32), mut cy: f32, item: &SceneItemVm) -> f32 {
        // A scene has one Environment, so its header has no Duplicate.
        let buttons_w = if item.environment { STEP_W } else { STEP_W * 2.0 };
        self.build_name_and_kind(tree, Rect::new(inner_x, cy, (inner_w - buttons_w - 4.0).max(0.0), ROW_H), item, true);
        let x = inner_x + inner_w - buttons_w;
        if !item.environment {
            let dup = tree.add_button_keyed(Some(self.content_parent), x, cy, STEP_W, ROW_H, btn_style(), "\u{29C9}", item_key(&item.id, "dup"));
            tree.set_name(dup, "scene_setup.properties.duplicate");
            self.header_buttons.duplicate.push((dup, item.id.clone()));
        }
        let remove = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + inner_w - STEP_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2715}",
            item_key(&item.id, "remove"),
        );
        tree.set_name(remove, "scene_setup.properties.remove");
        self.header_buttons.remove.push((remove, item.id.clone()));
        cy += ROW_H + ROW_GAP;
        if let Some(file) = item.aim_sun_file.as_ref().filter(|file| !file.is_empty()) {
            let id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                btn_style(),
                "Aim Sun at HDRI",
                item_key(&item.id, "aim_sun"),
            );
            tree.set_name(id, "scene_setup.lighting.aim_sun");
            self.header_buttons.aim_sun = Some((id, file.clone()));
            cy += ROW_H + ROW_GAP;
        }
        cy
    }

    /// The name button (a click renames) with the kind word right-aligned
    /// after it, skipped when the kind would repeat the name or `show_kind`
    /// is off.
    fn build_name_and_kind(&mut self, tree: &mut UITree, rect: Rect, item: &SceneItemVm, show_kind: bool) {
        let kind = item.kind.default_name();
        let kind_w = if kind == item.name || !show_kind {
            0.0
        } else {
            tree.text_width(kind, color::FONT_LABEL, crate::node::FontWeight::Regular) + GAP * 2.0
        };
        let name_w = (rect.width - kind_w).max(0.0);
        let name_id = tree.add_button_keyed(
            Some(self.content_parent),
            rect.x,
            rect.y,
            name_w,
            ROW_H,
            drag_value_style(),
            &item.name,
            item_key(&item.id, "name"),
        );
        tree.set_name(name_id, "scene_setup.properties.name_value");
        if kind_w > 0.0 {
            let kind_id = tree.add_label(Some(self.content_parent), rect.x + name_w, rect.y, kind_w - GAP, ROW_H, kind, kind_style());
            tree.set_name(kind_id, "scene_setup.properties.kind");
        }
        self.header_buttons.names.push((item.id.clone(), name_id, item.name.clone()));
    }

    /// A click on a header button, as the action it sends.
    pub(super) fn item_header_click(&self, node_id: NodeId, vm: &SceneSetupVm) -> Option<PanelAction> {
        let layer = || vm.layer_id.clone();
        if let Some((_, row)) = self.header_buttons.fields.iter().find(|(button, _)| *button == node_id) {
            // A disabled control's click does nothing, as its row's would.
            return field_row_action(&GraphParamTarget::GeneratorOf(layer()), row, node_id).into_iter().next();
        }
        if let Some((item, _, name)) = self.header_buttons.names.iter().find(|(_, button, _)| *button == node_id) {
            return Some(PanelAction::Root(RootAction::SceneRenameClicked(layer(), item.clone(), name.clone())));
        }
        if let Some((_, item)) = self.header_buttons.use_view.as_ref().filter(|(button, _)| *button == node_id) {
            return Some(PanelAction::Root(RootAction::SceneSetupCopyViewportCamera(layer(), item.clone())));
        }
        if self.header_buttons.draw_again == Some(node_id) {
            return Some(PanelAction::Root(RootAction::SceneDrawAgain(layer())));
        }
        if self.header_buttons.add_loop_camera == Some(node_id) {
            return Some(PanelAction::Project(ProjectAction::SceneAddLoopCamera(layer())));
        }
        if let Some((_, file)) = self.header_buttons.aim_sun.as_ref().filter(|(button, _)| *button == node_id) {
            return Some(PanelAction::Project(ProjectAction::SceneSetupAimSunAtHdri(layer(), file.clone())));
        }
        if let Some((_, item)) = self.header_buttons.frame.iter().find(|(button, _)| *button == node_id) {
            return Some(PanelAction::Project(ProjectAction::SceneSetupFrameSelected(layer(), item.clone())));
        }
        if let Some((_, item)) = self.header_buttons.duplicate.iter().find(|(button, _)| *button == node_id) {
            return Some(PanelAction::Project(ProjectAction::SceneDuplicate(layer(), item.clone())));
        }
        let (_, item) = self.header_buttons.remove.iter().find(|(button, _)| *button == node_id)?;
        Some(PanelAction::Project(ProjectAction::SceneRemove(layer(), vec![item.clone()])))
    }
}

/// Whether Frame applies to an item of `kind`, from its header button or the
/// keyboard: a Camera is the view itself, and the World and a Material have
/// no place to frame.
pub(super) fn can_frame(kind: ItemKind) -> bool {
    !matches!(kind, ItemKind::World | ItemKind::Material | ItemKind::Camera | ItemKind::Modifier)
}
