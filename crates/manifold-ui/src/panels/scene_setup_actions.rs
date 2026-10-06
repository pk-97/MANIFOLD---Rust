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
pub(crate) const KEY_ADD_FLUID: u64 = 80_021;

/// Button ids retained by the panel for click dispatch.
pub(crate) struct AddRowIds {
    pub object: NodeId,
    pub light: NodeId,
    pub plane: NodeId,
    pub fluid: NodeId,
}

/// Compact scene insertion row. Returns button ids and the y below the row.
pub(crate) fn build_add_action_row(
    tree: &mut UITree,
    parent: Option<NodeId>,
    inner_x: f32,
    inner_w: f32,
    cy: f32,
) -> (AddRowIds, f32) {
    let action_w = (inner_w - 3.0 * ROW_GAP) / 4.0;
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
    let fluid = tree.add_button_keyed(
        parent,
        inner_x + 3.0 * (action_w + ROW_GAP),
        cy,
        action_w,
        ROW_H,
        btn_style(),
        "+Water",
        KEY_ADD_FLUID,
    );
    tree.set_name(fluid, "scene_setup.add_fluid");
    (AddRowIds { object, light, plane, fluid }, cy + ROW_H)
}

/// Click dispatch for the add-action row: Object and Plane index off the
/// live `vm.object_count`, Light off `vm.light_count`.
pub(crate) fn add_row_click(
    object: Option<NodeId>,
    light: Option<NodeId>,
    plane: Option<NodeId>,
    fluid: Option<NodeId>,
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
    } else if fluid == Some(node_id) {
        Some(PanelAction::Project(ProjectAction::SceneSetupAddFluid(
            vm.layer_id.clone(),
            vm.scene_root_node_id,
        )))
    } else {
        None
    }
}
