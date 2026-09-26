use std::collections::BTreeMap;

use super::*;
use crate::command::Command;
use crate::commands::graph::test_support::{project_with_one_generator_layer, scene_param_meta};
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID,
    SerializedParamValue,
};
use manifold_core::{GraphTarget, NodeId};

fn render_scene_graph(objects: u32, occupied_next_slot: bool) -> EffectGraphDef {
    let mut render = node(10, "render", "node.render_scene");
    render.params.insert(
        "objects".into(),
        SerializedParamValue::Float {
            value: objects as f32,
        },
    );
    let mut nodes = vec![render, node(20, "camera", "node.orbit_camera")];
    let mut wires = Vec::new();
    for index in 0..objects {
        wires.push(wire(100 + index, "object", 10, &format!("object_{index}")));
        nodes.push(node(
            100 + index,
            &format!("existing_{index}"),
            "node.group",
        ));
    }
    if occupied_next_slot {
        wires.push(wire(300, "object", 10, &format!("object_{objects}")));
        nodes.push(node(300, "occupied", "node.group"));
    }
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes,
        wires,
    }
}

fn node(id: u32, stable_id: &str, type_id: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(stable_id),
        type_id: type_id.into(),
        handle: Some(stable_id.into()),
        params: BTreeMap::new(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.into(),
        to_node,
        to_port: to_port.into(),
    }
}

fn project_with_graph(def: EffectGraphDef) -> (manifold_core::project::Project, GraphTarget) {
    let (mut project, layer_id) = project_with_one_generator_layer();
    project.timeline.layers[0].gen_params_or_init();
    let target = GraphTarget::Generator(layer_id);
    project.graph_target_owner_mut(&target).unwrap().graph = Some(def);
    (project, target)
}

fn fresh_project() -> (manifold_core::project::Project, GraphTarget) {
    let (mut project, layer_id) = project_with_one_generator_layer();
    project.timeline.layers[0].gen_params_or_init();
    (project, GraphTarget::Generator(layer_id))
}

fn command(target: GraphTarget, catalog_default: EffectGraphDef) -> AddSceneFluidCommand {
    AddSceneFluidCommand::new(
        target,
        10,
        vec![
            scene_param_meta("fill_height", "Fill"),
            scene_param_meta("emission", "Legacy Emission"),
            scene_param_meta("inflow_speed", "Legacy Inflow Speed"),
        ],
        vec![
            scene_param_meta("pos_x", "Position X"),
            scene_param_meta("pos_y", "Position Y"),
            scene_param_meta("pos_z", "Position Z"),
            scene_param_meta("scale_x", "Scale X"),
            scene_param_meta("scale_y", "Scale Y"),
            scene_param_meta("scale_z", "Scale Z"),
            scene_param_meta("rot_x", "Rotation X"),
        ],
        vec![
            scene_param_meta("roughness", "Roughness"),
            scene_param_meta("volume_attenuation_distance", "Attenuation"),
        ],
        vec![scene_param_meta("cast_shadows", "Shadows")],
        catalog_default,
    )
    .with_role_metadata(vec![
        scene_param_meta("role", "Role"),
        scene_param_meta("enabled", "Enabled"),
        scene_param_meta("velocity_y", "Velocity Y"),
    ])
}

fn graph<'a>(
    project: &'a manifold_core::project::Project,
    target: &GraphTarget,
) -> &'a EffectGraphDef {
    project.graph_for_target(target, None).unwrap()
}

#[test]
fn scene_physics_add_fluid_appends_after_compound_slots() {
    let def = render_scene_graph(2, false);
    let (mut project, target) = project_with_graph(def.clone());
    let mut cmd = command(target.clone(), def);

    cmd.execute(&mut project);

    let result = graph(&project, &target);
    assert!(cmd.was_applied());
    assert_eq!(
        result
            .nodes
            .iter()
            .find(|node| node.id == 10)
            .unwrap()
            .params["objects"],
        SerializedParamValue::Float { value: 3.0 }
    );
    let group = result
        .nodes
        .iter()
        .find(|node| node.handle.as_deref() == Some("Fluid 1 Graph"))
        .unwrap();
    let body = group.group.as_deref().unwrap();
    assert_eq!(body.nodes.len(), 7);
    for type_id in [
        "node.fluid_surface",
        "node.transform_3d",
        "node.fluid_role_source",
        "node.pbr_material",
        "node.scene_object",
        "system.group_output",
    ] {
        assert!(body.nodes.iter().any(|node| node.type_id == type_id));
    }
    assert_eq!(
        body.nodes
            .iter()
            .filter(|node| node.type_id == "node.transform_3d")
            .count(),
        2
    );
    assert!(result.wires.iter().any(|wire| {
        wire.from_node == group.id && wire.to_node == 10 && wire.to_port == "object_2"
    }));
    assert_eq!(
        body.nodes
            .iter()
            .find(|node| node.type_id == "node.fluid_surface")
            .unwrap()
            .params["resolution"],
        SerializedParamValue::Int { value: 16 }
    );
    let source_id = body
        .nodes
        .iter()
        .find(|node| node.type_id == "node.transform_3d")
        .unwrap()
        .node_id
        .clone();
    assert!(result.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| {
        matches!(&binding.target, manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
            if node_id == &source_id && param == "rot_x")
    }));
    let fluid = body.nodes.iter().find(|node| node.type_id == "node.fluid_surface").unwrap();
    assert_eq!(fluid.params["emission"], SerializedParamValue::Float { value: 0.0 });
    assert_eq!(fluid.params["domain_size"], SerializedParamValue::Float { value: 4.0 });
    let domain = body
        .nodes
        .iter()
        .find(|node| node.handle.as_deref() == Some("Fluid 1 Domain"))
        .unwrap();
    assert_eq!(domain.params["pos_y"], SerializedParamValue::Float { value: 2.0 });
    assert_eq!(domain.params["scale_x"], SerializedParamValue::Float { value: 4.0 });
    assert!(body.wires.iter().any(|wire| {
        wire.from_node == domain.id
            && wire.from_port == "transform"
            && wire.to_node == fluid.id
            && wire.to_port == "domain"
    }));
    let role = body.nodes.iter().find(|node| node.type_id == "node.fluid_role_source").unwrap();
    assert_eq!(role.params["velocity_y"], SerializedParamValue::Float { value: -1.0 });
    assert!(body.wires.iter().any(|wire| wire.from_node == role.id && wire.from_port == "role" && wire.to_node == fluid.id && wire.to_port == "role_0"));
    assert!(!body.wires.iter().any(|wire| wire.to_node == fluid.id && wire.to_port == "emitter"));
    let fluid_node_id = &fluid.node_id;
    assert!(!result.preset_metadata.as_ref().unwrap().bindings.iter().any(|binding| matches!(
        &binding.target,
        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
            if node_id == fluid_node_id && matches!(param.as_str(), "emission" | "inflow_speed")
    )));
    let metadata = result.preset_metadata.as_ref().unwrap();
    let domain_section = metadata
        .params
        .iter()
        .filter(|param| param.section.as_deref() == Some("Fluid 1 - Domain"))
        .collect::<Vec<_>>();
    assert_eq!(domain_section.len(), 6);
    assert!(domain_section.iter().all(|param| {
        matches!(
            param.name.as_str(),
            "Position X" | "Position Y" | "Position Z" | "Width" | "Height" | "Depth"
        )
    }));
    assert!(domain_section
        .iter()
        .filter(|param| matches!(param.name.as_str(), "Width" | "Height" | "Depth"))
        .all(|param| param.min == 0.5 && param.max == 20.0));
    assert!(!metadata.bindings.iter().any(|binding| matches!(
        &binding.target,
        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
            if node_id == fluid_node_id && param == "domain_size"
    )));
}

#[test]
fn scene_physics_two_fluids_get_independent_ids_and_sections() {
    let def = render_scene_graph(0, false);
    let (mut project, target) = project_with_graph(def.clone());
    let mut first = command(target.clone(), def.clone());
    first.execute(&mut project);
    let mut second = command(target.clone(), def);
    second.execute(&mut project);

    let result = graph(&project, &target);
    let groups: Vec<_> = result
        .nodes
        .iter()
        .filter(|node| node.type_id == GROUP_TYPE_ID)
        .collect();
    assert_eq!(groups.len(), 2);
    assert_eq!(
        groups
            .iter()
            .map(|node| node.handle.as_deref().unwrap())
            .collect::<Vec<_>>(),
        vec!["Fluid 1 Graph", "Fluid 2 Graph"]
    );
    let sections: Vec<_> = result
        .preset_metadata
        .as_ref()
        .unwrap()
        .params
        .iter()
        .filter_map(|param| param.section.as_deref())
        .collect();
    assert!(sections.contains(&"Fluid 1 - Simulation"));
    assert!(sections.contains(&"Fluid 2 - Material"));
}

#[test]
fn scene_physics_undo_redo_preserves_fluid_ids_and_original_graph() {
    let def = render_scene_graph(1, false);
    let (mut project, target) = project_with_graph(def.clone());
    let mut cmd = command(target.clone(), def.clone());
    cmd.execute(&mut project);
    let after = graph(&project, &target).clone();

    cmd.undo(&mut project);
    assert_eq!(graph(&project, &target), &def);

    cmd.execute(&mut project);
    assert_eq!(graph(&project, &target), &after);
}

#[test]
fn scene_physics_fresh_preset_materializes_and_undo_restores_none() {
    let catalog = render_scene_graph(0, false);
    let (mut project, target) = fresh_project();
    let mut cmd = command(target.clone(), catalog.clone());

    assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
    cmd.execute(&mut project);
    assert!(cmd.was_applied());
    let after = graph(&project, &target).clone();
    cmd.undo(&mut project);
    assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
    cmd.execute(&mut project);
    assert_eq!(graph(&project, &target), &after);
}

#[test]
fn scene_physics_rejection_does_not_bump_fresh_instance_versions() {
    let mut catalog = render_scene_graph(0, false);
    catalog.nodes[0].params.insert(
        "objects".into(),
        SerializedParamValue::Float { value: f32::NAN },
    );
    let (mut project, target) = fresh_project();
    let (before_graph, before_structure) = {
        let owner = project.graph_target_owner(&target).unwrap();
        (owner.graph_version, owner.graph_structure_version)
    };
    let mut cmd = command(target.clone(), catalog);
    cmd.execute(&mut project);
    assert!(!cmd.was_applied());
    let owner = project.graph_target_owner(&target).unwrap();
    assert!(owner.graph.is_none());
    assert_eq!(owner.graph_version, before_graph);
    assert_eq!(owner.graph_structure_version, before_structure);
}

#[test]
fn scene_physics_invalid_root_and_occupied_slot_reject_atomically() {
    let mut malformed = render_scene_graph(0, false);
    malformed.nodes[0].type_id = "node.camera".into();
    let (mut project, target) = project_with_graph(malformed.clone());
    let mut cmd = command(target.clone(), malformed.clone());
    cmd.execute(&mut project);
    assert!(!cmd.was_applied());
    assert_eq!(graph(&project, &target), &malformed);

    let occupied = render_scene_graph(2, true);
    let (mut project, target) = project_with_graph(occupied.clone());
    let mut cmd = command(target.clone(), occupied.clone());
    cmd.execute(&mut project);
    assert!(!cmd.was_applied());
    assert_eq!(graph(&project, &target), &occupied);
}

#[test]
fn scene_physics_add_fluid_eighth_id_exhaustion_is_atomic() {
    let mut catalog = render_scene_graph(0, false);
    catalog.nodes.push(node(u32::MAX - 7, "last_existing", "node.value"));
    let (mut project, target) = fresh_project();
    let before = project.clone();
    let mut cmd = command(target, catalog);
    cmd.execute(&mut project);
    assert!(!cmd.was_applied());
    assert_eq!(serde_json::to_value(&project).unwrap(), serde_json::to_value(before).unwrap());
}
