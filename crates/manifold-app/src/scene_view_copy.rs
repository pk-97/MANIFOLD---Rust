//! "Use this view" (SCENE_SURFACES D14). The editor viewport's camera lives
//! on the UI thread (`Workspace::viewport_session`), so the app resolves it
//! when the Camera header's button is clicked and writes the scene camera
//! once, as one undoable command. Moving the viewport never writes the
//! project.

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId};
use manifold_editing::command::Command;
use manifold_editing::commands::graph::CopyViewportCameraCommand;
use manifold_node_engine::scene::viewport_camera::ViewportCamera;
use manifold_nodes_scene::node_graph::scene_vm::{CameraVm, SceneVm};
use manifold_nodes_scene::node_graph::view_copy::{ViewCopyCamera, camera_writes_from_view};

use crate::content_command::ContentCommand;
use crate::ui_bridge::project::{apply_scene_param_write, scene_binding_id};

const NO_VIEWPORT: &str = "Open this scene in the 3D viewport first";

/// The viewport camera, when the open viewport shows `layer_id`'s scene.
pub(crate) fn live_view(ws: &crate::workspace::Workspace, layer_id: &LayerId) -> Option<ViewportCamera> {
    let session = ws.viewport_session.as_ref()?;
    match &ws.viewport_target {
        Some((GraphTarget::Generator(layer), _)) if layer == layer_id => Some(session.config.camera),
        _ => None,
    }
}

/// The command that puts `layer_id`'s scene camera at `view`. Params that
/// already hold their value are left out; an empty command means the camera
/// already shows the view.
pub(crate) fn build(project: &Project, layer_id: &LayerId, view: &ViewportCamera) -> Result<CopyViewportCameraCommand, &'static str> {
    let default = crate::ui_bridge::generator_catalog_default(project, layer_id).ok_or("This layer has no scene")?;
    let effective = project.timeline.find_layer_by_id(layer_id)
        .and_then(|(_, layer)| layer.generator_graph().cloned())
        .unwrap_or(default);
    let vm = SceneVm::from_def(&effective).ok_or("This layer has no scene")?;
    let (kind, node) = match &vm.camera {
        CameraVm::Orbit(row) => (ViewCopyCamera::Orbit, row.node_doc_id),
        CameraVm::Free(row) => (ViewCopyCamera::Free, row.node_doc_id),
        _ => return Err("Use this view is not supported for this camera type"),
    };
    let current = |param: &str| current_value(project, layer_id, &effective, node, param);
    let look_y = current("look_y").unwrap_or(0.0);
    let writes = camera_writes_from_view(kind, view, look_y)
        .into_iter()
        .filter(|(param, value)| current(param) != Some(*value))
        .filter_map(|(param, value)| apply_scene_param_write(project, layer_id, Vec::new(), node, param, value))
        .map(|write| -> Box<dyn Command> { write })
        .collect();
    Ok(CopyViewportCameraCommand::new(writes))
}

/// What the scene renders for a root camera param: the card slot's base
/// value when a binding owns it, else the graph literal.
fn current_value(project: &Project, layer_id: &LayerId, def: &EffectGraphDef, node: u32, param: &str) -> Option<f32> {
    if let Some(id) = scene_binding_id(def, &[], node, param) {
        let instance = project.graph_target_owner(&GraphTarget::Generator(layer_id.clone()))?;
        return instance.params.contains(id.as_str()).then(|| instance.get_base_param(id.as_str()));
    }
    match def.nodes.iter().find(|n| n.id == node)?.params.get(param)? {
        SerializedParamValue::Float { value } => Some(*value),
        _ => None,
    }
}

/// The click: copy the view, or say why not. Returns whether the project
/// changed.
pub(crate) fn run(
    project: &mut Project,
    content_tx: &crossbeam_channel::Sender<ContentCommand>,
    ui: &mut crate::ui_root::UIRoot,
    stamps: &mut crate::frame_plan::UiStamps,
    layer_id: &LayerId,
    view: Option<&ViewportCamera>,
) -> bool {
    let Some(view) = view else {
        ui.toast.show(NO_VIEWPORT);
        return false;
    };
    match build(project, layer_id, view) {
        Ok(command) if command.is_empty() => false,
        Ok(command) => {
            let mut command: Box<dyn Command> = Box::new(command);
            command.execute(project);
            stamps.bump(command.touches());
            ContentCommand::send(content_tx, ContentCommand::Execute(command));
            true
        }
        Err(reason) => {
            ui.toast.show(reason);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::change::{ChangeSet, Part};
    use manifold_core::types::LayerType;

    fn scene_project(preset: &str) -> (Project, LayerId, u32) {
        let mut project = Project::default();
        let idx = project.timeline.add_layer(preset, LayerType::Generator, PresetTypeId::from_string(preset.to_string()));
        let layer_id = project.timeline.layers[idx].layer_id.clone();
        let def = effective(&project, &layer_id);
        let camera = match SceneVm::from_def(&def).expect("bundled scene").camera {
            CameraVm::Orbit(row) => row.node_doc_id,
            CameraVm::Free(row) => row.node_doc_id,
            other => panic!("{preset} camera is {other:?}"),
        };
        (project, layer_id, camera)
    }

    fn effective(project: &Project, layer_id: &LayerId) -> EffectGraphDef {
        let default = crate::ui_bridge::generator_catalog_default(project, layer_id).expect("bundled preset");
        project.timeline.find_layer_by_id(layer_id)
            .and_then(|(_, layer)| layer.generator_graph().cloned())
            .unwrap_or(default)
    }

    fn values(project: &Project, layer_id: &LayerId, camera: u32, params: &[&str]) -> Vec<Option<f32>> {
        let def = effective(project, layer_id);
        params.iter().map(|param| current_value(project, layer_id, &def, camera, param)).collect()
    }

    fn moved_view() -> ViewportCamera {
        let mut view = ViewportCamera { target: [0.4, 1.2, -0.3], fov_y: 0.7, ..Default::default() };
        view.orbit(140.0, -35.0, 0.006);
        view.dolly(1.0, 0.08);
        view
    }

    #[test]
    fn orbit_copy_is_a_values_edit_and_one_undo_restores_every_param() {
        const PARAMS: &[&str] = &["orbit", "tilt", "distance", "look_y", "fov_y", "roll"];
        let (mut project, layer_id, camera) = scene_project("Scene");
        let before = values(&project, &layer_id, camera, PARAMS);
        let view = moved_view();
        let mut command = build(&project, &layer_id, &view).unwrap();
        assert!(!command.is_empty());
        assert_eq!(command.touches(), ChangeSet::of(&[Part::Values]), "every orbit param is a card slot");
        command.execute(&mut project);
        let after = values(&project, &layer_id, camera, PARAMS);
        let def = effective(&project, &layer_id);
        let look_y = current_value(&project, &layer_id, &def, camera, "look_y").unwrap();
        let p = manifold_nodes_scene::node_graph::view_copy::orbit_from_view(view.to_camera().pos, view.to_camera().fwd, look_y);
        assert_eq!(after[..5], [Some(p.orbit), Some(p.tilt), Some(p.distance), Some(p.look_y), Some(view.fov_y)]);
        assert_ne!(after, before);
        command.undo(&mut project);
        assert_eq!(values(&project, &layer_id, camera, PARAMS), before);
        assert!(build(&project, &layer_id, &view).is_ok());
    }

    #[test]
    fn free_copy_is_exact() {
        const PARAMS: &[&str] = &["pos_x", "pos_y", "pos_z", "yaw", "pitch", "fov_y"];
        let (mut project, layer_id, camera) = scene_project("Ocean");
        let before = values(&project, &layer_id, camera, PARAMS);
        let view = moved_view();
        let mut command = build(&project, &layer_id, &view).unwrap();
        command.execute(&mut project);
        let [x, y, z] = view.position();
        assert_eq!(
            values(&project, &layer_id, camera, PARAMS),
            [Some(x), Some(y), Some(z), Some(view.yaw), Some(view.pitch), Some(view.fov_y)],
        );
        command.undo(&mut project);
        assert_eq!(values(&project, &layer_id, camera, PARAMS), before);
    }

    #[test]
    fn copying_the_same_view_twice_writes_nothing_the_second_time() {
        let (mut project, layer_id, _) = scene_project("Scene");
        let view = moved_view();
        build(&project, &layer_id, &view).unwrap().execute(&mut project);
        assert!(build(&project, &layer_id, &view).unwrap().is_empty());
    }
}
