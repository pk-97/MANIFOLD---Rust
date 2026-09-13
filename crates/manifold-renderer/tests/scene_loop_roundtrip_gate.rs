//! Scene Loop v3 authoring and round-trip acceptance.
//!
//! Legacy fixed-row descriptor expectations moved to the load-only migration
//! tests. These checks exercise the real bundled recipe, host insertion,
//! compiler preparation, and serialized value preservation.

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::InsertSceneModifierCommand;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers;
use manifold_renderer::node_graph::PrimitiveRegistry;

#[path = "common/scene_modifier.rs"]
mod common;

const HOST: &str = include_str!("fixtures/scene-modifiers/nested_multimaterial_v2.json");

fn host() -> EffectGraphDef {
    serde_json::from_str(HOST).expect("nested v2 host parses")
}

fn apply_loop(project: &mut Project, def: EffectGraphDef) -> (manifold_core::LayerId, usize) {
    let idx = project.timeline.add_layer(
        "Loop Gate",
        LayerType::Generator,
        PresetTypeId::from_string("LoopGateScene".to_string()),
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(def);
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let owner = project.timeline.layers[idx].generator_graph().unwrap().clone();
    let instance = prepare_new_scene_modifier(
        &owner,
        &common::stock_recipe("SceneLoop"),
        manifold_core::NodeId::new("roundtrip-loop"),
        common::render_scene(&owner),
        manifold_core::scene_modifier_preset::SceneTargetSelection::AllObjects,
    )
    .expect("loop recipe prepares");
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let mut command = InsertSceneModifierCommand::new(
        project,
        target,
        &owner,
        0,
        instance,
    )
    .expect("loop command prepares");
    command.execute(project);
    assert!(command.was_applied(), "loop insertion applies");
    (layer_id, idx)
}

#[test]
fn scene_loop_v3_roundtrip_keeps_values_and_expanded_graph() {
    let mut project = Project::default();
    let (layer_id, idx) = apply_loop(&mut project, host());
    let graph = project.timeline.layers[idx].generator_graph().unwrap();
    assert_eq!(graph.scene_modifiers.len(), 1);
    let instance = &graph.scene_modifiers[0];
    assert_eq!(instance.id.as_str(), "roundtrip-loop");
    assert!(instance.mesh_frames.is_empty(), "loop has no mesh source frames");
    let expanded = prepare_scene_modifiers(graph, &PrimitiveRegistry::with_builtin())
        .expect("loop expands through compiler");
    assert!(expanded.def.scene_modifiers.is_empty());
    let camera_route = expanded
        .routes
        .iter()
        .find(|route| {
            route.modifier_id == manifold_core::NodeId::new("roundtrip-loop")
                && route.local.node.as_str() == "loop_camera"
        })
        .expect("loop camera route survives expansion");
    let camera_id = &camera_route.copies[0].node_id;
    let camera = expanded
        .def
        .nodes
        .iter()
        .find(|node| &node.node_id == camera_id)
        .expect("generated loop camera survives expansion");
    assert!(camera.params.contains_key("pattern_length"));
    assert!(expanded
        .def
        .wires
        .iter()
        .any(|wire| wire.from_node == camera.id && wire.to_port == "camera"));

    let path = std::env::temp_dir().join(format!(
        "manifold_scene_loop_v3_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v3 project");
    let reloaded = manifold_io::loader::load_project(&path).expect("reload v3 project");
    let graph = reloaded
        .timeline
        .layers
        .iter()
        .find(|layer| layer.layer_id == layer_id)
        .and_then(|layer| layer.generator_graph())
        .expect("reloaded graph");
    assert_eq!(graph.scene_modifiers.len(), 1);
    assert_eq!(graph.scene_modifiers[0].id.as_str(), "roundtrip-loop");
    let phase = common::find_node(&graph.scene_modifiers[0].graph.nodes, "loop_phase")
        .expect("phase in saved modifier");
    assert_eq!(phase.params.get("bars"), Some(&SerializedParamValue::Int { value: 8 }));
    let reopened = prepare_scene_modifiers(graph, &PrimitiveRegistry::with_builtin())
        .expect("saved loop prepares again");
    assert_eq!(reopened.def, expanded.def);
    assert_eq!(reopened.routes, expanded.routes);
    std::fs::remove_file(path).ok();
}

#[test]
fn scene_loop_preparation_is_idempotent_after_flatten() {
    let mut project = Project::default();
    let (_layer_id, idx) = apply_loop(&mut project, host());
    let graph = project.timeline.layers[idx].generator_graph().unwrap();
    let first = prepare_scene_modifiers(graph, &PrimitiveRegistry::with_builtin())
        .expect("first preparation");
    let second = prepare_scene_modifiers(graph, &PrimitiveRegistry::with_builtin())
        .expect("second preparation");
    assert_eq!(first.def, second.def);
    assert_eq!(first.routes, second.routes);
}
