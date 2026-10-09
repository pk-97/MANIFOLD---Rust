//! Scene Setup camera preparation is one content-owned, undoable graph edit.
use manifold_core::{GraphTarget, LayerId, SceneNodeRef};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_index::FlatSceneIndex;
use manifold_editing::command::Command;
use manifold_nodes_scene::node_graph::scene_camera;

pub(crate) fn needs_setup(graph: &EffectGraphDef, scene_id: u32) -> bool {
    let Some(scene) = graph.nodes.iter().find(|node| node.id == scene_id) else { return true; };
    let Ok(index) = FlatSceneIndex::build(graph) else { return true; };
    let scene = SceneNodeRef { scope: Vec::new(), node: scene.node_id.clone() };
    let has_lens = index.input(&scene, "camera").ok().flatten().is_some_and(|wire|
        index.flat.nodes.iter().any(|node| node.id == wire.from_node && node.type_id == "node.camera_lens"
            && index.flat.wires.iter().filter(|edge| edge.to_node == node.id && edge.to_port == "camera"
                && index.flat.nodes.iter().any(|source| source.id == edge.from_node)).count() == 1));
    !has_lens || scene_camera::camera_effect_controls(&index, &scene).len() != 2
}

pub(crate) fn build_action(project: &Project, layer_id: LayerId) -> Result<Box<dyn Command>, String> {
    let target = GraphTarget::Generator(layer_id.clone());
    let before = project.graph_target_owner(&target).ok_or("Scene is no longer available")?.clone();
    let mut graph = crate::graph_target::resolve(project, &target)
        .ok_or("Scene graph is unavailable")?.clone();
    if !scene_camera::restore_camera_effects(&mut graph)? {
        return Err("Camera effects are already set up for this scene.".into());
    }
    manifold_nodes_scene::node_graph::scene_exposure::migrate_scene_exposures(&mut graph);
    let mut after = before.clone();
    after.graph = Some(graph);
    after.refresh_manifest_from_graph();
    Ok(Box::new(crate::generator_change::ReplaceGeneratorStateCommand::new(
        layer_id, before, after, "Set up scene camera effects",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{PresetTypeId, layer::Layer};

    #[test]
    fn bundled_native_scenes_share_camera_infrastructure() {
        use manifold_core::preset_def::PresetKind;
        use manifold_nodes::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
        let mut checked = 0;
        for id in bundled_preset_type_ids(PresetKind::Generator) {
            let def = bundled_preset_def(&id).unwrap();
            let Some(vm) = manifold_nodes_scene::node_graph::scene_vm::SceneVm::from_def(&def) else { continue; };
            assert!(!needs_setup(&def, vm.scene_root_node_id), "{id} has incomplete camera infrastructure");
            checked += 1;
        }
        assert!(checked >= 12, "expected the bundled 3D scene family");
    }

    #[test]
    fn camera_setup_preserves_host_state_and_round_trips_undo() {
        let mut layer = Layer::new_generator("Scene".into(), PresetTypeId::new("Scene"), 0);
        let id = layer.layer_id.clone();
        let host = layer.gen_params_mut().unwrap();
        host.graph = Some(serde_json::from_value(serde_json::json!({"version":2,"nodes":[
            {"id":1,"nodeId":"camera","typeId":"node.orbit_camera"},
            {"id":2,"nodeId":"scene","typeId":"node.render_scene"},
            {"id":3,"nodeId":"final","typeId":"system.final_output"}
        ],"wires":[
            {"fromNode":1,"fromPort":"out","toNode":2,"toPort":"camera"},
            {"fromNode":2,"fromPort":"color","toNode":3,"toPort":"in"}
        ]})).unwrap());
        host.enabled = false;
        let before = serde_json::to_value(&*host).unwrap();
        let mut project = Project::default();
        project.timeline.layers.push(layer);
        let mut command = build_action(&project, id.clone()).unwrap();
        command.execute(&mut project);
        assert!(command.was_applied());
        let target = GraphTarget::Generator(id);
        let after = project.graph_target_owner(&target).unwrap();
        assert!(!after.enabled);
        assert!(!needs_setup(after.graph.as_ref().unwrap(), 2));
        let after = serde_json::to_value(after).unwrap();
        command.undo(&mut project);
        assert_eq!(serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap(), before);
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(serde_json::to_value(project.graph_target_owner(&target).unwrap()).unwrap(), after);
    }
}
