//! The scene setup panel's compact add-action row (BUG-hlw8). Lives apart
//! from `scene_setup_panel.rs` — that file is under the godfile-decomposition
//! line ceiling, so row builders for its buttons go in sibling modules like
//! this one, not into the capped file (the `scene_setup_skin.rs` precedent).

use crate::ProjectAction;
use crate::node::NodeId;
use crate::tree::UITree;

use super::PanelAction;
use super::scene_setup_panel::{ROW_GAP, ROW_H, SceneSetupVm, btn_style};

pub(crate) const KEY_ADD_OBJECT: u64 = 80_014;
pub(crate) const KEY_ADD_LIGHT: u64 = 80_015;
/// BUG-hlw8 "+ Plane" button — dispatches `AddSceneLayerPlaneCommand`.
pub(crate) const KEY_ADD_PLANE: u64 = 80_020;

/// The button ids the row built, handed back for the panel's click-dispatch
/// fields (`add_object_id`/`add_light_id`/`add_plane_id`).
pub(crate) struct AddRowIds {
    pub object: NodeId,
    pub light: NodeId,
    pub plane: NodeId,
}

/// D6/BUG-hlw8: compact action row — Object, Light, and Layer Plane (a
/// skinned plane mesh) all share the same live `next_index` source
/// (`vm.object_count`). The compact Import Model button that used to render
/// here was unreachable: `import_model_id` is overwritten by the Objects
/// section header's Import button, so it emitted no action. Returns the ids
/// and the y below the row.
pub(crate) fn build_add_action_row(
    tree: &mut UITree,
    parent: Option<NodeId>,
    inner_x: f32,
    inner_w: f32,
    cy: f32,
) -> (AddRowIds, f32) {
    let action_w = (inner_w - 2.0 * ROW_GAP) / 3.0;
    let object = tree.add_button_keyed(
        parent,
        inner_x,
        cy,
        action_w,
        ROW_H,
        btn_style(),
        "+ Object",
        KEY_ADD_OBJECT,
    );
    let light = tree.add_button_keyed(
        parent,
        inner_x + action_w + ROW_GAP,
        cy,
        action_w,
        ROW_H,
        btn_style(),
        "+ Light",
        KEY_ADD_LIGHT,
    );
    let plane = tree.add_button_keyed(
        parent,
        inner_x + 2.0 * (action_w + ROW_GAP),
        cy,
        action_w,
        ROW_H,
        btn_style(),
        "+ Plane",
        KEY_ADD_PLANE,
    );
    (AddRowIds { object, light, plane }, cy + ROW_H)
}

/// Click dispatch for the add-action row: Object and Plane index off the
/// live `vm.object_count`, Light off `vm.light_count`.
pub(crate) fn add_row_click(
    object: Option<NodeId>,
    light: Option<NodeId>,
    plane: Option<NodeId>,
    node_id: NodeId,
    vm: &SceneSetupVm,
) -> Option<PanelAction> {
    if object == Some(node_id) {
        Some(PanelAction::Project(ProjectAction::SceneSetupAddObject(
            vm.layer_id.clone(),
            vm.scene_root_node_id,
            vm.object_count as u32,
        )))
    } else if light == Some(node_id) {
        Some(PanelAction::Project(ProjectAction::SceneSetupAddLight(
            vm.layer_id.clone(),
            vm.scene_root_node_id,
            vm.light_count as u32,
        )))
    } else if plane == Some(node_id) {
        Some(PanelAction::Project(ProjectAction::SceneSetupAddLayerPlane(
            vm.layer_id.clone(),
            vm.scene_root_node_id,
            vm.object_count as u32,
        )))
    } else {
        None
    }
}
