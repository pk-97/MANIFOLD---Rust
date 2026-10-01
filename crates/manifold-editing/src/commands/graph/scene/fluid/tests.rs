use std::collections::BTreeMap;

use super::*;
use crate::command::Command;
use crate::commands::graph::test_support::{project_with_one_generator_layer, scene_param_meta};
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_TYPE_ID,
    SerializedParamValue,
};
use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
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
        flip_scene_fluid_template(),
        catalog_default,
    )
    .with_role_metadata(vec![
        scene_param_meta("role", "Role"),
        scene_param_meta("enabled", "Enabled"),
        scene_param_meta("velocity_y", "Velocity Y"),
    ])
    .with_world_metadata(world_metadata())
}

fn world_metadata() -> Vec<manifold_core::scene_exposure::SceneParamMetadata> {
    [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0), ("speed", 1.0), ("reset", 0.0)]
        .into_iter().map(|(name, value)| {
            let mut metadata = scene_param_meta(name, name);
            metadata.default_value = SerializedParamValue::Float { value };
            metadata.is_trigger = name == "reset";
            metadata
        }).collect()
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
    assert_eq!(body.nodes.len(), 8);
    for type_id in [
        FLIP_DOMAIN_TYPE_ID,
        "node.transform_3d",
        "node.fluid_role_source",
        "node.pbr_material",
        "node.scene_object",
        "system.group_input",
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
            .find(|node| node.type_id == FLIP_DOMAIN_TYPE_ID)
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
    let fluid = body.nodes.iter().find(|node| node.type_id == FLIP_DOMAIN_TYPE_ID).unwrap();
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
    assert_eq!(result.nodes.iter().filter(|node| node.type_id == "node.physics_world").count(), 1);
    assert_eq!(result.nodes.iter().filter(|node| node.type_id == "node.value").count(), 5,
        "additional liquid domains reuse the existing World sources");
}

#[test]
fn scene_physics_duplicate_fluid_keeps_shared_world_controls() {
    use crate::commands::graph::DuplicateSceneObjectCommand;

    let baseline = render_scene_graph(0, false);
    let (mut project, target) = project_with_graph(baseline.clone());
    let mut add = command(target.clone(), baseline.clone());
    add.execute(&mut project);
    assert!(add.was_applied());
    let before = graph(&project, &target).clone();
    let original = before.nodes.iter().find(|node| node.type_id == GROUP_TYPE_ID).unwrap();
    let incoming: Vec<_> = before.wires.iter().filter(|wire| wire.to_node == original.id).cloned().collect();
    assert_eq!(incoming.len(), 5);

    let mut duplicate = DuplicateSceneObjectCommand::new(target.clone(), vec![], 10, 0, baseline);
    duplicate.execute(&mut project);
    assert!(duplicate.was_applied(), "{:?}", duplicate.rejection_reason());
    let after = graph(&project, &target).clone();
    let copied = after.nodes.iter().find(|node| node.type_id == GROUP_TYPE_ID && node.id != original.id).unwrap();
    for source in incoming {
        assert!(after.wires.contains(&source), "original keeps its source");
        assert!(after.wires.contains(&manifold_core::effect_graph_def::EffectGraphWire {
            to_node: copied.id, ..source
        }), "copy keeps the same World signal");
    }
    assert_eq!(after.nodes.iter().filter(|node| node.type_id == "node.value").count(), 5);
    let reloaded: manifold_core::project::Project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(graph(&reloaded, &target), &after);
    assert!(manifold_core::flatten::flatten_groups(&after).is_ok());
    duplicate.undo(&mut project);
    assert_eq!(graph(&project, &target), &before);
    duplicate.execute(&mut project);
    assert_eq!(graph(&project, &target), &after);
}

#[test]
fn scene_physics_add_fluid_preserves_world_controls_and_authored_wires() {
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_core::scene_exposure::stamp_scene_node_exposures;

    let mut def = render_scene_graph(0, false);
    let mut world = node(30, "world", "node.physics_world");
    world.params.insert("speed".into(), SerializedParamValue::Float { value: 1.75 });
    world.exposed_params.insert("speed".into());
    def.nodes.push(world);
    def.nodes.push(node(31, "authored_driver", "node.value"));
    def.wires.push(wire(31, "out", 30, "gravity_x"));
    stamp_scene_node_exposures(&mut def, 30, "World", &world_metadata());
    let original_metadata = def.preset_metadata.clone().unwrap();
    let (mut project, target) = project_with_graph(def.clone());
    project.graph_target_owner_mut(&target).unwrap().refresh_manifest_from_graph();
    assert!(project.graph_target_owner_mut(&target).unwrap().set_base_param("30_speed", 2.0));
    let mut before_speed = project.graph_target_owner(&target).unwrap().params.get("30_speed").unwrap().clone();
    // Manifest refresh clears this frame-local gesture latch, not authored state.
    before_speed.touched = false;
    let audio = vec![manifold_core::audio_mod::ParameterAudioMod::new(
        "30_speed".into(), manifold_core::id::AudioSendId::new("scene-audio"),
        manifold_core::audio_mod::AudioFeature::default(),
    )];
    project.graph_target_owner_mut(&target).unwrap().audio_mods = Some(audio.clone());
    let mut cmd = command(target.clone(), def.clone());
    cmd.execute(&mut project);
    assert!(cmd.was_applied(), "{:?}", cmd.rejection_reason());
    let result = graph(&project, &target);
    let group_node = result.nodes.iter().find(|node| node.type_id == GROUP_TYPE_ID).unwrap();
    let group = group_node.group.as_deref().unwrap();
    let input = group.nodes.iter().find(|node| node.type_id == "system.group_input").unwrap();
    let fluid = group.nodes.iter().find(|node| node.type_id == FLIP_DOMAIN_TYPE_ID).unwrap();
    let metadata = result.preset_metadata.as_ref().unwrap();
    for (world_param, fluid_param) in [("gravity_x", "gravity_x"), ("gravity_y", "gravity"),
        ("gravity_z", "gravity_z"), ("speed", "speed"), ("reset", "reset")]
    {
        let world_wire = result.wires.iter().find(|wire| wire.to_node == 30 && wire.to_port == world_param).unwrap();
        let fluid_wire = result.wires.iter().find(|wire| wire.to_node == group_node.id && wire.to_port == world_param).unwrap();
        assert_eq!((world_wire.from_node, &world_wire.from_port), (fluid_wire.from_node, &fluid_wire.from_port));
        assert!(group.wires.iter().any(|wire| wire.from_node == input.id && wire.from_port == world_param
            && wire.to_node == fluid.id && wire.to_port == fluid_param));
        let id = format!("30_{world_param}");
        let before = original_metadata.bindings.iter().find(|binding| binding.id == id).unwrap();
        let after = metadata.bindings.iter().find(|binding| binding.id == id).unwrap();
        let mut expected = before.clone();
        if world_param == "gravity_x" {
            assert_eq!(world_wire.from_node, 31, "existing graph modulation stays authoritative");
        } else {
            let source = result.nodes.iter().find(|node| node.id == world_wire.from_node).unwrap();
            expected.target = BindingTarget::Node { node_id: source.node_id.clone(), param: "value".into() };
            if world_param == "speed" {
                assert!(source.exposed_params.contains("value"));
                assert_eq!(source.params["value"], SerializedParamValue::Float { value: 1.75 });
            }
        }
        assert_eq!(after, &expected, "binding identity and conversion must survive lifting");
        assert_eq!(metadata.params.iter().find(|spec| spec.id == id), original_metadata.params.iter().find(|spec| spec.id == id));
    }
    assert_eq!(project.graph_target_owner(&target).unwrap().params.get("30_speed"), Some(&before_speed));
    assert_eq!(project.graph_target_owner(&target).unwrap().audio_mods.as_ref(), Some(&audio));
    let after = result.clone();
    let reloaded: manifold_core::project::Project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(graph(&reloaded, &target), &after);
    assert_eq!(reloaded.graph_target_owner(&target).unwrap().get_base_param("30_speed"), 2.0);
    assert_eq!(reloaded.graph_target_owner(&target).unwrap().audio_mods.as_ref(), Some(&audio));
    cmd.undo(&mut project);
    assert_eq!(graph(&project, &target), &def);
    cmd.execute(&mut project);
    assert_eq!(graph(&project, &target), &after);
}

#[test]
fn scene_physics_add_fluid_rejects_ambiguous_world_controls_atomically() {
    for duplicate_world in [false, true] {
        let mut def = render_scene_graph(0, false);
        def.nodes.push(node(30, "world", "node.physics_world"));
        if duplicate_world {
            def.nodes.push(node(31, "second_world", "node.physics_world"));
        } else {
            def.nodes.push(node(31, "driver", "node.value"));
            def.wires.push(wire(31, "out", 30, "speed"));
            def.wires.push(wire(31, "out", 30, "speed"));
        }
        let (mut project, target) = project_with_graph(def.clone());
        let before = serde_json::to_value(&project).unwrap();
        let mut cmd = command(target, def);
        cmd.execute(&mut project);
        assert!(!cmd.was_applied());
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
    }
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

fn gpu_template() -> LiquidTemplate {
    use manifold_core::effect_graph_def::{
        GROUP_OUTPUT_TYPE_ID, GroupDef, GroupInterface,
    };
    let mut live = node(1, "live_matter", GROUP_TYPE_ID);
    live.handle = Some("Live Matter".into());
    live.group = Some(Box::new(GroupDef {
        interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
        nodes: vec![node(1, "matter_domain", manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID)],
        wires: vec![],
        tint: None,
    }));
    let mut surface = node(2, "liquid_surface", GROUP_TYPE_ID);
    surface.handle = Some("Liquid Surface".into());
    surface.group = Some(Box::new(GroupDef {
        interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
        nodes: vec![node(1, "surface_mesh", "node.value"), node(2, "surface_out", "node.value")],
        wires: vec![wire(1, "out", 2, "in")],
        tint: None,
    }));
    let mut object = node(3, "fluid_object", "node.scene_object");
    object.handle = Some(String::new());
    let mut output = node(4, "fluid_output", GROUP_OUTPUT_TYPE_ID);
    output.handle = None;
    LiquidTemplate {
        nodes: vec![live, surface, object, output],
        wires: vec![wire(1, "frame", 2, "frame"), wire(2, "vertices", 3, "vertices"), wire(3, "object", 4, "object")],
        output_node: 4,
        group_id_slot: 0,
        exposures: vec![TemplateExposure { node: 3, set: ExposureSet::Object, section: None }],
        world_control_target: None,
    }
}

fn add_undo_redo_reload(template: LiquidTemplate, domain_type: &str) {
    let def = render_scene_graph(1, false);
    let (mut project, target) = project_with_graph(def.clone());
    let mut cmd = command(target.clone(), def.clone());
    cmd.template = template;
    cmd.execute(&mut project);
    assert!(cmd.was_applied(), "{:?}", cmd.rejection_reason());
    let added = graph(&project, &target).clone();
    let group = added.nodes.iter().find(|node| node.handle.as_deref() == Some("Fluid 1 Graph")).unwrap();
    fn holds(nodes: &[EffectGraphNode], type_id: &str) -> bool {
        nodes.iter().any(|node| {
            node.type_id == type_id || node.group.as_deref().is_some_and(|g| holds(&g.nodes, type_id))
        })
    }
    assert!(holds(&group.group.as_deref().unwrap().nodes, domain_type));
    let mut ids = Vec::new();
    fn collect(nodes: &[EffectGraphNode], ids: &mut Vec<u32>) {
        for node in nodes {
            ids.push(node.id);
            if let Some(group) = node.group.as_deref() {
                collect(&group.nodes, ids);
            }
        }
    }
    collect(&added.nodes, &mut ids);
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "document ids must stay unique across nesting");

    cmd.undo(&mut project);
    assert_eq!(graph(&project, &target), &def);
    cmd.execute(&mut project);
    assert!(cmd.was_applied(), "{:?}", cmd.rejection_reason());
    assert_eq!(graph(&project, &target), &added);

    let reloaded: manifold_core::project::Project =
        serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
    assert_eq!(graph(&reloaded, &target), &added);
}

#[test]
fn scene_physics_add_fluid_template_undo_reload() {
    add_undo_redo_reload(flip_scene_fluid_template(), FLIP_DOMAIN_TYPE_ID);
    add_undo_redo_reload(gpu_template(), manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID);
}
