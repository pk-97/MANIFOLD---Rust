//! The selected item's header (SCENE_SURFACES D8): its name once (a rename
//! button where a rename command exists), its kind, and copy and delete
//! where commands exist. Objects copy and delete, forces copy and delete,
//! lights delete only; Camera, Physics and World have neither.

use super::*;
use manifold_foundation::scene_surface::{ObjectKind, SceneItemKind};

const KEY_FORCE_COPY: u64 = 93_000_300;
const KEY_FORCE_DELETE: u64 = 93_000_301;

/// The kind word shown beside the name.
pub(super) fn kind_label(kind: SceneItemKind) -> &'static str {
    match kind {
        SceneItemKind::Camera => "Camera",
        SceneItemKind::Light => "Light",
        SceneItemKind::Object(ObjectKind::Mesh) => "Object",
        SceneItemKind::Object(ObjectKind::Plane) => "Plane",
        SceneItemKind::Object(ObjectKind::Water) => "Water",
        SceneItemKind::Object(ObjectKind::Model) => "Model",
        SceneItemKind::Force => "Force",
        SceneItemKind::Physics => "Physics",
        SceneItemKind::World => "World",
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

impl ScenePanel {
    pub(super) fn build_item_header(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        vm: &SceneSetupVm,
        item: &SceneItemVm,
    ) -> f32 {
        match &item.selection {
            SceneSelection::Object(id) => {
                let Some(row) = vm.objects.iter().find_map(|object| match object {
                    ObjectRowVm::Known(row) if row.object_node_id == *id => Some(row.as_ref()),
                    _ => None,
                }) else {
                    return cy;
                };
                self.build_object_header(tree, (inner_x, inner_w), cy, row, item.kind)
            }
            SceneSelection::Light(id) => {
                let Some(row) = vm.lights.iter().find_map(|light| match light {
                    LightRowVm::Known(row) if row.node_doc_id == *id => Some(row.as_ref()),
                    _ => None,
                }) else {
                    return cy;
                };
                let name_w = inner_w - STEP_W - 4.0;
                let name_id = self.build_name_and_kind(
                    tree,
                    Rect::new(inner_x, cy, name_w, ROW_H),
                    &row.name,
                    item.kind,
                    Some(light_key(row.index, LIGHT_OFF_NAME)),
                );
                self.light_name_ids.push((row.node_doc_id, name_id, row.name.clone()));
                let remove_id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x + name_w + 4.0,
                    cy,
                    STEP_W,
                    ROW_H,
                    btn_style(),
                    "\u{2715}",
                    light_key(row.index, LIGHT_OFF_REMOVE),
                );
                tree.set_name(remove_id, "scene_setup.properties.remove");
                self.light_remove_ids.push((remove_id, row.index));
                cy + ROW_H + ROW_GAP
            }
            SceneSelection::Force(id) => {
                let name_w = inner_w - STEP_W * 2.0 - 4.0;
                self.build_name_and_kind(tree, Rect::new(inner_x, cy, name_w, ROW_H), &item.name, item.kind, None);
                let mut x = inner_x + name_w + 4.0;
                for (glyph, key, name) in [
                    ("\u{29C9}", KEY_FORCE_COPY, "scene_setup.properties.duplicate"),
                    ("\u{2715}", KEY_FORCE_DELETE, "scene_setup.properties.remove"),
                ] {
                    let button = tree.add_button_keyed(Some(self.content_parent), x, cy, STEP_W, ROW_H, btn_style(), glyph, key);
                    tree.set_name(button, name);
                    if key == KEY_FORCE_COPY {
                        self.force_duplicate_ids.push((button, id.clone()));
                    } else {
                        self.force_remove_ids.push((button, id.clone()));
                    }
                    x += STEP_W;
                }
                cy + ROW_H + ROW_GAP
            }
            SceneSelection::Camera | SceneSelection::World | SceneSelection::Physics(_) => {
                self.build_name_and_kind(tree, Rect::new(inner_x, cy, inner_w, ROW_H), &item.name, item.kind, None);
                cy + ROW_H + ROW_GAP
            }
            SceneSelection::OutlinerFold(_) => cy,
        }
    }

    /// The name (a rename button when `rename_key` is set, else a label)
    /// with the kind word right-aligned after it, skipped when the kind
    /// would repeat the name.
    fn build_name_and_kind(
        &mut self,
        tree: &mut UITree,
        rect: Rect,
        name: &str,
        kind: SceneItemKind,
        rename_key: Option<u64>,
    ) -> NodeId {
        let kind = kind_label(kind);
        let kind_w = if kind == name {
            0.0
        } else {
            tree.text_width(kind, color::FONT_LABEL, crate::node::FontWeight::Regular) + GAP * 2.0
        };
        let name_w = (rect.width - kind_w).max(0.0);
        let name_id = match rename_key {
            Some(key) => tree.add_button_keyed(
                Some(self.content_parent), rect.x, rect.y, name_w, ROW_H, drag_value_style(), name, key,
            ),
            None => tree.add_label(Some(self.content_parent), rect.x, rect.y, name_w, ROW_H, name, header_label_style()),
        };
        tree.set_name(name_id, "scene_setup.properties.name_value");
        if kind_w > 0.0 {
            let kind_id = tree.add_label(
                Some(self.content_parent), rect.x + name_w, rect.y, kind_w - GAP, ROW_H, kind, kind_style(),
            );
            tree.set_name(kind_id, "scene_setup.properties.kind");
        }
        name_id
    }

    /// Object header: rename, kind and Frame, then a family child's Hide or
    /// a regular object's copy and delete (a family parent has no copy).
    fn build_object_header(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        row: &ObjectKnownRow,
        kind: SceneItemKind,
    ) -> f32 {
        // Frame and Hide need two icon-cell widths for their labels.
        let frame_w = STEP_W * 2.0;
        let family_parent = self.is_family_parent(row);
        let show_hide = row.look_mesh.is_some();
        let show_duplicate = row.look_mesh.is_none() && !family_parent;
        let show_remove = row.look_mesh.is_none();
        let btn_w = frame_w
            + frame_w * (show_hide as u8) as f32
            + STEP_W * (show_duplicate as u8 + show_remove as u8) as f32;
        let name_w = inner_w - btn_w - 8.0;
        let name_id = self.build_name_and_kind(
            tree,
            Rect::new(inner_x, cy, name_w, ROW_H),
            &row.name,
            kind,
            Some(obj_key(row.object_node_id as usize, OBJ_OFF_NAME)),
        );
        self.object_name_ids.push((row.object_node_id, name_id, row.name.clone()));

        let frame_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0,
            cy,
            frame_w,
            ROW_H,
            btn_style(),
            "Frame",
            obj_key(row.object_node_id as usize, OBJ_OFF_FRAME),
        );
        self.object_frame_ids.push((frame_id, row.object_node_id));
        tree.set_name(frame_id, "scene_setup.properties.frame");

        let mut action_x = inner_x + name_w + 4.0 + frame_w;
        if show_hide {
            let hide_id = tree.add_button_keyed(
                Some(self.content_parent),
                action_x,
                cy,
                frame_w,
                ROW_H,
                if row.visible.driven { driven_label_style() } else { btn_style() },
                "Hide",
                obj_key(row.object_node_id as usize, OBJ_OFF_HIDE),
            );
            tree.set_name(hide_id, "scene_setup.properties.hide");
            if !row.visible.driven {
                self.object_hide_ids.push((hide_id, row.visible.clone()));
            }
            action_x += frame_w;
        }
        let submesh = row.parent_group_id.is_some() && !row.is_group;
        if show_duplicate {
            let dup_id = tree.add_button_keyed(
                Some(self.content_parent),
                action_x,
                cy,
                STEP_W,
                ROW_H,
                btn_style(),
                "\u{29C9}",
                obj_key(row.object_node_id as usize, OBJ_OFF_REMOVE) + 1,
            );
            tree.set_name(dup_id, "scene_setup.properties.duplicate");
            if submesh {
                self.submesh_duplicate_ids.push((dup_id, row.index));
            } else {
                self.object_duplicate_ids.push((dup_id, row.index));
            }
            action_x += STEP_W;
        }
        if show_remove {
            let remove_id = tree.add_button_keyed(
                Some(self.content_parent),
                action_x,
                cy,
                STEP_W,
                ROW_H,
                btn_style(),
                "\u{2715}",
                obj_key(row.object_node_id as usize, OBJ_OFF_REMOVE),
            );
            tree.set_name(remove_id, "scene_setup.properties.remove");
            if submesh {
                self.submesh_remove_ids.push((remove_id, row.index));
            } else {
                self.object_remove_ids.push((remove_id, row.index));
            }
        }
        cy + ROW_H + ROW_GAP
    }

    /// The force header's copy and delete.
    pub(super) fn item_header_click(&self, node_id: NodeId, vm: &SceneSetupVm) -> Option<PanelAction> {
        if let Some((_, id)) = self.force_duplicate_ids.iter().find(|(button, _)| *button == node_id) {
            return Some(PanelAction::Project(ProjectAction::SceneModifiersDuplicate(vm.layer_id.clone(), vec![id.clone()])));
        }
        let (_, id) = self.force_remove_ids.iter().find(|(button, _)| *button == node_id)?;
        Some(PanelAction::Project(ProjectAction::SceneModifierRemove(vm.layer_id.clone(), id.clone())))
    }
}
