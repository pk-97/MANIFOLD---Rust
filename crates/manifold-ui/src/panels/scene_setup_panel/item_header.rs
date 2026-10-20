//! The selected item's header (SCENE_SURFACES D8): its name once (a rename
//! button where a rename command exists), its kind, and copy and delete
//! where commands exist. Objects copy and delete, forces copy and delete,
//! lights delete only; Camera, Lighting, Physics and World have neither.
//! Each light's header heads its sub-section inside Lighting. The Camera
//! header also carries "Use this view" (D14).

use super::*;
use manifold_foundation::scene_surface::{ObjectKind, SceneItemKind};

const KEY_FORCE_COPY: u64 = 93_000_300;
const KEY_FORCE_DELETE: u64 = 93_000_301;
const KEY_USE_VIEW: u64 = 93_000_302;
const USE_VIEW: &str = "Use this view";

/// Header buttons built this frame, cleared on every rebuild.
#[derive(Default)]
pub(super) struct HeaderButtons {
    force_duplicate: Vec<(NodeId, FoundationNodeId)>,
    force_remove: Vec<(NodeId, FoundationNodeId)>,
    use_view: Option<NodeId>,
}

impl HeaderButtons {
    pub(super) fn clear(&mut self) {
        self.force_duplicate.clear();
        self.force_remove.clear();
        self.use_view = None;
    }
}

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
                self.build_light_header(tree, (inner_x, inner_w), cy, (row.node_doc_id, row.index, &row.name))
            }
            SceneSelection::Lighting => {
                let name = tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &item.name, header_label_style());
                tree.set_name(name, "scene_setup.properties.name_value");
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
                        self.header_buttons.force_duplicate.push((button, id.clone()));
                    } else {
                        self.header_buttons.force_remove.push((button, id.clone()));
                    }
                    x += STEP_W;
                }
                cy + ROW_H + ROW_GAP
            }
            SceneSelection::Camera => self.build_camera_header(tree, (inner_x, inner_w), cy, vm, item),
            SceneSelection::World | SceneSelection::Physics(_) => {
                self.build_name_and_kind(tree, Rect::new(inner_x, cy, inner_w, ROW_H), &item.name, item.kind, None);
                cy + ROW_H + ROW_GAP
            }
            SceneSelection::OutlinerFold(_) => cy,
        }
    }

    /// One light's name (click to rename) and kind, and its delete button.
    pub(super) fn build_light_header(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        (doc_id, index, name): (u32, usize, &str),
    ) -> f32 {
        let name_w = inner_w - STEP_W - 4.0;
        let name_id = self.build_name_and_kind(
            tree,
            Rect::new(inner_x, cy, name_w, ROW_H),
            name,
            SceneItemKind::Light,
            Some(light_key(index, LIGHT_OFF_NAME)),
        );
        self.light_name_ids.push((doc_id, name_id, name.to_string()));
        let remove_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2715}",
            light_key(index, LIGHT_OFF_REMOVE),
        );
        tree.set_name(remove_id, "scene_setup.properties.remove");
        self.light_remove_ids.push((remove_id, index));
        cy + ROW_H + ROW_GAP
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

    /// Camera header: the name, then "Use this view" for orbit and free
    /// cameras. Look-at, loop and custom cameras show it dimmed with the
    /// reason, since they aim at their own target.
    fn build_camera_header(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        cy: f32,
        vm: &SceneSetupVm,
        item: &SceneItemVm,
    ) -> f32 {
        let supported = matches!(vm.camera, CameraRowVm::Orbit | CameraRowVm::Free);
        let button_w = tree.text_width(USE_VIEW, color::FONT_LABEL, crate::node::FontWeight::Regular) + 16.0;
        let name_w = (inner_w - button_w - 4.0).max(0.0);
        self.build_name_and_kind(tree, Rect::new(inner_x, cy, name_w, ROW_H), &item.name, item.kind, None);
        let x = inner_x + name_w + 4.0;
        let button = if supported {
            let id = tree.add_button_keyed(Some(self.content_parent), x, cy, button_w, ROW_H, btn_style(), USE_VIEW, KEY_USE_VIEW);
            self.header_buttons.use_view = Some(id);
            id
        } else {
            let style = UIStyle { text_align: TextAlign::Center, ..driven_label_style() };
            tree.add_label(Some(self.content_parent), x, cy, button_w, ROW_H, USE_VIEW, style)
        };
        tree.set_name(button, "scene_setup.properties.use_view");
        let cy = cy + ROW_H + ROW_GAP;
        if supported || matches!(vm.camera, CameraRowVm::None) {
            return cy;
        }
        let reason = tree.add_label(
            Some(self.content_parent), inner_x, cy, inner_w, ROW_H,
            "Use this view: not supported for this camera type", label_style(),
        );
        tree.set_name(reason, "scene_setup.properties.use_view_reason");
        cy + ROW_H
    }

    /// The force header's copy and delete, and the Camera's "Use this view".
    pub(super) fn item_header_click(&self, node_id: NodeId, vm: &SceneSetupVm) -> Option<PanelAction> {
        if self.header_buttons.use_view == Some(node_id) {
            return Some(PanelAction::Root(RootAction::SceneSetupCopyViewportCamera(vm.layer_id.clone())));
        }
        if let Some((_, id)) = self.header_buttons.force_duplicate.iter().find(|(button, _)| *button == node_id) {
            return Some(PanelAction::Project(ProjectAction::SceneModifiersDuplicate(vm.layer_id.clone(), vec![id.clone()])));
        }
        let (_, id) = self.header_buttons.force_remove.iter().find(|(button, _)| *button == node_id)?;
        Some(PanelAction::Project(ProjectAction::SceneModifierRemove(vm.layer_id.clone(), id.clone())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{azalea_shaped_vm, live};
    use crate::input::Modifiers;
    use crate::node::Vec2;

    fn camera_header(camera: Option<CameraRowVm>) -> (ScenePanel, UITree) {
        let mut vm = azalea_shaped_vm();
        if let Some(camera) = camera {
            vm.camera = camera;
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(live(vm));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 1000.0));
        (panel, tree)
    }

    fn named(tree: &UITree, name: &str) -> Vec<NodeId> {
        (0..tree.count()).map(|i| tree.id_at(i)).filter(|&id| tree.name_of(id) == Some(name)).collect()
    }

    #[test]
    fn use_this_view_copies_the_viewport_for_an_orbit_camera() {
        let (mut panel, mut tree) = camera_header(None);
        let button = panel.header_buttons.use_view.expect("orbit camera has Use this view");
        assert_eq!(tree.get_node(button).unwrap().text.as_deref(), Some(USE_VIEW));
        assert!(named(&tree, "scene_setup.properties.use_view_reason").is_empty());
        let (consumed, actions) = panel.handle_event(
            &UIEvent::Click { node_id: button, pos: Vec2::ZERO, modifiers: Modifiers::default() },
            &mut tree,
        );
        assert!(consumed);
        assert!(matches!(actions.as_slice(),
            [PanelAction::Root(RootAction::SceneSetupCopyViewportCamera(layer))] if *layer == LayerId::new("layer-1")));
    }

    #[test]
    fn use_this_view_is_dimmed_with_its_reason_for_other_cameras() {
        let (panel, tree) = camera_header(Some(CameraRowVm::Custom));
        assert!(panel.header_buttons.use_view.is_none(), "no click target");
        assert_eq!(named(&tree, "scene_setup.properties.use_view").len(), 1, "still shown, dimmed");
        let reason = named(&tree, "scene_setup.properties.use_view_reason");
        assert_eq!(
            tree.get_node(reason[0]).unwrap().text.as_deref(),
            Some("Use this view: not supported for this camera type"),
        );
    }
}
