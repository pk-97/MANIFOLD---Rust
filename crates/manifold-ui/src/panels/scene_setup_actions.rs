//! The Scene tab's Objects add menu (SCENE_SURFACES D8): one "+" on the
//! Objects group opens Object, Plane, Water and Import Model…, each the
//! existing add command.

use crate::ProjectAction;

use super::PanelAction;
use super::scene_setup_panel::SceneSetupVm;

/// The Objects menu for `vm`, in display order. Object and Plane take the
/// next object slot.
pub fn add_object_menu(vm: &SceneSetupVm) -> Vec<(&'static str, PanelAction)> {
    let layer = || vm.layer_id.clone();
    let root = vm.scene_root_node_id;
    let next = vm.object_count as u32;
    vec![
        ("Object", PanelAction::Project(ProjectAction::SceneSetupAddObject(layer(), root, next))),
        ("Plane", PanelAction::Project(ProjectAction::SceneSetupAddLayerPlane(layer(), root, next))),
        ("Water", PanelAction::Project(ProjectAction::SceneSetupAddFluid(layer(), root))),
        ("Import Model\u{2026}", PanelAction::Project(ProjectAction::SceneSetupImportModelClicked(layer(), root))),
    ]
}
