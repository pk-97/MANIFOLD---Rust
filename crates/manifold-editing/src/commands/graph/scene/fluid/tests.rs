//! Adding a liquid through the one scene insertion: identity, sections,
//! the shared World controls, atomic refusals and undo, against a local
//! liquid template shaped like Dam Break Matter's (its domain nested in
//! Live Matter).

use std::collections::BTreeMap;

use crate::command::Command;
use crate::commands::graph::test_support::{project_with_one_generator_layer, scene_param_meta};
use crate::scene_template::InsertSceneTemplateCommand;
use manifold_core::effect_graph_def::{
    EFFECT_GRAPH_VERSION, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID,
    GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID;
use manifold_core::scene_exposure::{NodeParamMetadata, stamp_scene_node_exposures_into};
use manifold_core::scene_modifier_preset::SceneNodeRef;
use manifold_core::scene_template::{HANDLE_TOKEN, SceneTemplate};
use manifold_core::{GraphTarget, NodeId};

/// The Physics World's shared controls and the ports the test liquid's domain
/// takes them on, as the node registry declares them (a real liquid domain
/// declares the same five); every other type declares nothing here, so a
/// lifted control keeps its saved spec.
fn node_params(type_id: &str) -> Vec<NodeParamMetadata> {
    match type_id {
        "node.physics_world" => world_metadata(),
        MATTER_DOMAIN_TYPE_ID => ["gravity_x", "gravity", "gravity_z", "speed", "reset"]
            .into_iter()
            .map(|name| scene_param_meta(name, name))
            .collect(),
        _ => Vec::new(),
    }
}

inventory::submit! {
    manifold_core::param_lookup::NodeParamSource { params_for_type: node_params }
}

fn render_scene_graph(objects: u32, occupied_next_slot: bool) -> EffectGraphDef {
    let mut render = node(10, "render", "node.render_scene");
    render.params.insert("objects".into(), SerializedParamValue::Float { value: objects as f32 });
    let mut nodes = vec![render, node(20, "camera", "node.orbit_camera")];
    let mut wires = Vec::new();
    for index in 0..objects {
        wires.push(wire(100 + index, "object", 10, &format!("object_{index}")));
        nodes.push(node(100 + index, &format!("existing_{index}"), "node.group"));
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
    EffectGraphWire { from_node, from_port: from_port.into(), to_node, to_port: to_port.into() }
}

fn project_with_graph(def: EffectGraphDef) -> (manifold_core::project::Project, GraphTarget) {
    let (mut project, layer_id) = project_with_one_generator_layer();
    let target = GraphTarget::Generator(layer_id);
    project.graph_target_owner_mut(&target).unwrap().graph = Some(def);
    (project, target)
}

fn fresh_project() -> (manifold_core::project::Project, GraphTarget) {
    let (project, layer_id) = project_with_one_generator_layer();
    (project, GraphTarget::Generator(layer_id))
}

fn world_metadata() -> Vec<NodeParamMetadata> {
    [("gravity_x", 0.0), ("gravity_y", -9.81), ("gravity_z", 0.0), ("speed", 1.0), ("reset", 0.0)]
        .into_iter()
        .map(|(name, value)| {
            let mut metadata = scene_param_meta(name, name);
            metadata.default_value = SerializedParamValue::Float { value };
            metadata.is_trigger = name == "reset";
            metadata
        })
        .collect()
}

fn template_metadata(display_name: &str) -> PresetMetadata {
    PresetMetadata {
        shape: None,
        id: manifold_core::PresetTypeId::from_string(format!("__template_{display_name}")),
        display_name: display_name.into(),
        category: "Geometry".into(),
        osc_prefix: "scene".into(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: Vec::new(),
        bindings: Vec::new(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_modifier: None,
        scene_bounds: None,
    }
}

/// A liquid whose domain sits inside Live Matter, which already owns one
/// input sentinel, beside a surface, a material and the water's object.
/// Its card: the domain's fill under Simulation, the material's roughness,
/// the object's shadows.
fn liquid_template() -> SceneTemplate {
    let port = |name: &str| InterfacePortDef { name: name.into(), port_type: "Any".into() };
    let mut live = node(1, "live_matter", GROUP_TYPE_ID);
    live.handle = Some(format!("{HANDLE_TOKEN} Live Matter"));
    live.group = Some(Box::new(GroupDef {
        interface: GroupInterface { inputs: vec![], outputs: vec![port("frame")], params: vec![] },
        nodes: vec![
            node(11, "matter_domain", MATTER_DOMAIN_TYPE_ID),
            node(12, "live_input", GROUP_INPUT_TYPE_ID),
            node(13, "live_output", GROUP_OUTPUT_TYPE_ID),
        ],
        wires: vec![wire(11, "frame", 13, "frame")],
        tint: None,
    }));
    let mut surface = node(2, "liquid_surface", GROUP_TYPE_ID);
    surface.handle = Some(format!("{HANDLE_TOKEN} Liquid Surface"));
    surface.group = Some(Box::new(GroupDef {
        interface: GroupInterface { inputs: vec![port("frame")], outputs: vec![port("vertices")], params: vec![] },
        nodes: vec![
            node(21, "surface_mesh", "node.value"),
            node(22, "surface_input", GROUP_INPUT_TYPE_ID),
            node(23, "surface_output", GROUP_OUTPUT_TYPE_ID),
        ],
        wires: vec![wire(22, "frame", 21, "in"), wire(21, "out", 23, "vertices")],
        tint: None,
    }));
    let mut object = node(3, "fluid_object", "node.scene_object");
    object.handle = Some(HANDLE_TOKEN.into());
    let mut output = node(4, "fluid_output", GROUP_OUTPUT_TYPE_ID);
    output.handle = None;
    let material = node(5, "water_material", "node.pbr_material");
    let mut metadata = template_metadata("Fluid");
    let domain = &live.group.as_deref().unwrap().nodes[0];
    for (at, section, params) in [
        (domain, format!("{HANDLE_TOKEN} - Simulation"), vec![scene_param_meta("fill_height", "Fill")]),
        (&material, format!("{HANDLE_TOKEN} - Material"), vec![scene_param_meta("roughness", "Roughness")]),
        (&object, HANDLE_TOKEN.to_owned(), vec![scene_param_meta("cast_shadows", "Shadows")]),
    ] {
        stamp_scene_node_exposures_into(
            &mut metadata.params,
            &mut metadata.bindings,
            at.id,
            &at.node_id,
            &section,
            &params,
            &at.params,
        );
    }
    let mut group = node(6, "fluid", GROUP_TYPE_ID);
    group.handle = Some(format!("{HANDLE_TOKEN} Graph"));
    group.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![],
            outputs: vec![InterfacePortDef { name: "object".into(), port_type: "SceneObject".into() }],
            params: vec![],
        },
        nodes: vec![live, surface, object, output, material],
        wires: vec![wire(1, "frame", 2, "frame"), wire(2, "vertices", 3, "vertices"), wire(3, "object", 4, "object")],
        tint: None,
    }));
    SceneTemplate { group, metadata }
}

fn insert(
    project: &manifold_core::project::Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
    template: SceneTemplate,
) -> Result<InsertSceneTemplateCommand, String> {
    let scene = SceneNodeRef { scope: Vec::new(), node: NodeId::new("render") };
    InsertSceneTemplateCommand::new(project, target.clone(), scene, None, template, catalog_default)
        .map_err(|error| error.message)
}

fn add(
    project: &mut manifold_core::project::Project,
    target: &GraphTarget,
    catalog_default: &EffectGraphDef,
) -> InsertSceneTemplateCommand {
    let mut command = insert(project, target, catalog_default, liquid_template()).expect("the liquid inserts");
    command.execute(project);
    assert!(command.was_applied(), "{:?}", command.rejection_reason());
    command
}

fn graph<'a>(project: &'a manifold_core::project::Project, target: &GraphTarget) -> &'a EffectGraphDef {
    project.graph_for_target(target, None).unwrap()
}

fn node_and_group<'a>(
    nodes: &'a [EffectGraphNode],
    type_id: &str,
) -> Option<(&'a EffectGraphNode, &'a [EffectGraphNode], &'a [EffectGraphWire])> {
    for node in nodes {
        let Some(group) = node.group.as_deref() else { continue };
        if let Some(found) = group.nodes.iter().find(|candidate| candidate.type_id == type_id) {
            return Some((found, &group.nodes, &group.wires));
        }
        if let Some(found) = node_and_group(&group.nodes, type_id) {
            return Some(found);
        }
    }
    None
}

#[test]
fn scene_physics_two_fluids_get_independent_ids_and_sections() {
    let def = render_scene_graph(0, false);
    let (mut project, target) = project_with_graph(def.clone());
    add(&mut project, &target, &def);
    add(&mut project, &target, &def);

    let result = graph(&project, &target);
    let groups: Vec<_> = result.nodes.iter().filter(|node| node.type_id == GROUP_TYPE_ID).collect();
    assert_eq!(
        groups.iter().map(|node| node.handle.as_deref().unwrap()).collect::<Vec<_>>(),
        vec!["Fluid 1 Graph", "Fluid 2 Graph"]
    );
    let sections: Vec<_> =
        result.preset_metadata.as_ref().unwrap().params.iter().filter_map(|param| param.section.as_deref()).collect();
    assert!(sections.contains(&"Fluid 1 - Simulation"));
    assert!(sections.contains(&"Fluid 2 - Material"));
    assert_eq!(result.nodes.iter().filter(|node| node.type_id == "node.physics_world").count(), 1);
    assert_eq!(
        result.nodes.iter().filter(|node| node.type_id == "node.value").count(),
        5,
        "additional liquid domains reuse the existing World sources"
    );
}

#[test]
fn scene_physics_template_name_prefix_preserves_collision_numbering() {
    let mut def = render_scene_graph(0, false);
    let mut occupied = node(500, "occupied_water", GROUP_TYPE_ID);
    occupied.handle = Some("Water 1".into());
    def.nodes.push(occupied);

    let (mut project, target) = project_with_graph(def.clone());
    let mut template = liquid_template();
    template.metadata.display_name = "Water".into();
    let mut command = insert(&project, &target, &def, template).unwrap();
    command.execute(&mut project);

    let result = graph(&project, &target);
    assert!(command.was_applied());
    assert!(result.nodes.iter().any(|node| node.handle.as_deref() == Some("Water 2 Graph")));
    assert!(
        result
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .any(|param| { param.section.as_deref() == Some("Water 2 - Simulation") })
    );
}

#[test]
fn scene_physics_duplicate_fluid_keeps_shared_world_controls() {
    use crate::commands::graph::DuplicateSceneObjectCommand;

    let baseline = render_scene_graph(0, false);
    let (mut project, target) = project_with_graph(baseline.clone());
    add(&mut project, &target, &baseline);
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
        assert!(
            after.wires.contains(&EffectGraphWire { to_node: copied.id, ..source }),
            "copy keeps the same World signal"
        );
    }
    assert_eq!(after.nodes.iter().filter(|node| node.type_id == "node.value").count(), 5);
    let reloaded: manifold_core::project::Project =
        serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
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
    // Manifest refresh clears this frame-local gesture latch, not authored
    // state; the lifted control's lookup moves to its value source.
    before_speed.touched = false;
    before_speed.source = manifold_core::params::SpecSource::Unresolved;
    let audio = vec![manifold_core::audio_mod::ParameterAudioMod::new(
        "30_speed".into(),
        manifold_core::id::AudioSendId::new("scene-audio"),
        manifold_core::audio_mod::AudioFeature::default(),
    )];
    project.graph_target_owner_mut(&target).unwrap().audio_mods = Some(audio.clone());
    let mut cmd = add(&mut project, &target, &def);
    let result = graph(&project, &target);
    let group_node = result.nodes.iter().find(|node| node.type_id == GROUP_TYPE_ID).unwrap();
    let group = group_node.group.as_deref().unwrap();
    let (fluid, domain_nodes, domain_wires) = node_and_group(&group.nodes, MATTER_DOMAIN_TYPE_ID).unwrap();
    let input = domain_nodes.iter().find(|node| node.type_id == GROUP_INPUT_TYPE_ID).unwrap();
    let metadata = result.preset_metadata.as_ref().unwrap();
    for (world_param, fluid_param) in
        [("gravity_x", "gravity_x"), ("gravity_y", "gravity"), ("gravity_z", "gravity_z"), ("speed", "speed"), ("reset", "reset")]
    {
        let world_wire = result.wires.iter().find(|wire| wire.to_node == 30 && wire.to_port == world_param).unwrap();
        let fluid_wire =
            result.wires.iter().find(|wire| wire.to_node == group_node.id && wire.to_port == world_param).unwrap();
        assert_eq!((world_wire.from_node, &world_wire.from_port), (fluid_wire.from_node, &fluid_wire.from_port));
        assert!(domain_wires.iter().any(|wire| wire.from_node == input.id
            && wire.from_port == world_param
            && wire.to_node == fluid.id
            && wire.to_port == fluid_param));
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
        assert_eq!(
            metadata.params.iter().find(|spec| spec.id == id),
            original_metadata.params.iter().find(|spec| spec.id == id)
        );
    }
    assert_eq!(project.graph_target_owner(&target).unwrap().params.get("30_speed"), Some(&before_speed));
    assert_eq!(project.graph_target_owner(&target).unwrap().audio_mods.as_ref(), Some(&audio));
    let after = result.clone();
    let reloaded: manifold_core::project::Project =
        serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
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
        let (project, target) = project_with_graph(def.clone());
        let before = serde_json::to_value(&project).unwrap();
        assert!(insert(&project, &target, &def, liquid_template()).is_err());
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
    }
}

#[test]
fn scene_physics_undo_redo_preserves_fluid_ids_and_original_graph() {
    let def = render_scene_graph(1, false);
    let (mut project, target) = project_with_graph(def.clone());
    let mut cmd = add(&mut project, &target, &def);
    let after = graph(&project, &target).clone();

    cmd.undo(&mut project);
    assert_eq!(graph(&project, &target), &def);

    cmd.execute(&mut project);
    assert_eq!(graph(&project, &target), &after);
}

/// A liquid whose domain declares none of the World's ports is refused, so no
/// wire lands on a port the domain does not take.
#[test]
fn scene_physics_liquid_without_world_ports_is_refused() {
    let def = render_scene_graph(1, false);
    let (project, target) = project_with_graph(def);
    let mut template = liquid_template();
    let live = template.group.group.as_mut().unwrap().nodes.iter_mut().find(|node| node.id == 1).unwrap();
    live.group.as_mut().unwrap().nodes[0].type_id = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID.into();
    assert_eq!(
        insert(&project, &target, graph(&project, &target), template).err().as_deref(),
        Some("This liquid does not take the Physics World's controls")
    );
}

#[test]
fn scene_physics_fresh_preset_materializes_and_undo_restores_none() {
    let catalog = render_scene_graph(0, false);
    let (mut project, target) = fresh_project();
    assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
    let mut cmd = add(&mut project, &target, &catalog);
    let after = graph(&project, &target).clone();
    cmd.undo(&mut project);
    assert!(project.graph_target_owner(&target).unwrap().graph.is_none());
    cmd.execute(&mut project);
    assert_eq!(graph(&project, &target), &after);
}

#[test]
fn scene_physics_rejection_does_not_bump_fresh_instance_versions() {
    let mut catalog = render_scene_graph(0, false);
    catalog.nodes[0].params.insert("objects".into(), SerializedParamValue::Float { value: f32::NAN });
    let (project, target) = fresh_project();
    let (before_graph, before_structure) = {
        let owner = project.graph_target_owner(&target).unwrap();
        (owner.graph_version, owner.graph_structure_version)
    };
    assert!(insert(&project, &target, &catalog, liquid_template()).is_err());
    let owner = project.graph_target_owner(&target).unwrap();
    assert!(owner.graph.is_none());
    assert_eq!(owner.graph_version, before_graph);
    assert_eq!(owner.graph_structure_version, before_structure);
}

#[test]
fn scene_physics_invalid_root_and_occupied_slot_reject_atomically() {
    let mut malformed = render_scene_graph(0, false);
    malformed.nodes[0].type_id = "node.camera".into();
    let (project, target) = project_with_graph(malformed.clone());
    assert!(insert(&project, &target, &malformed, liquid_template()).is_err());
    assert_eq!(graph(&project, &target), &malformed);

    let occupied = render_scene_graph(2, true);
    let (project, target) = project_with_graph(occupied.clone());
    assert!(insert(&project, &target, &occupied, liquid_template()).is_err());
    assert_eq!(graph(&project, &target), &occupied);
}

#[test]
fn scene_physics_add_fluid_id_exhaustion_is_atomic() {
    let mut catalog = render_scene_graph(0, false);
    catalog.nodes.push(node(u32::MAX - 7, "last_existing", "node.value"));
    let (project, target) = fresh_project();
    let before = serde_json::to_value(&project).unwrap();
    assert_eq!(
        insert(&project, &target, &catalog, liquid_template()).err().as_deref(),
        Some("Scene document id space is exhausted")
    );
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
}

/// The domain sits inside Live Matter, so the World controls cross two
/// group boundaries: the liquid's group, then Live Matter's own sentinel.
#[test]
fn scene_physics_add_fluid_wires_world_controls_into_nested_gpu_domain() {
    let def = render_scene_graph(0, false);
    let (mut project, target) = project_with_graph(def.clone());
    add(&mut project, &target, &def);
    let result = graph(&project, &target);
    let world = result.nodes.iter().find(|node| node.type_id == "node.physics_world").unwrap();
    let fluid_group = result.nodes.iter().find(|node| node.handle.as_deref() == Some("Fluid 1 Graph")).unwrap();
    let outer = fluid_group.group.as_deref().unwrap();
    let outer_input = outer.nodes.iter().find(|node| node.type_id == GROUP_INPUT_TYPE_ID).unwrap();
    let live = outer.nodes.iter().find(|node| node.handle.as_deref() == Some("Fluid 1 Live Matter")).unwrap();
    let inner = live.group.as_deref().unwrap();
    let inner_inputs: Vec<_> = inner.nodes.iter().filter(|node| node.type_id == GROUP_INPUT_TYPE_ID).collect();
    assert_eq!(inner_inputs.len(), 1, "Live Matter's own sentinel is reused");
    let domain = inner.nodes.iter().find(|node| node.type_id == MATTER_DOMAIN_TYPE_ID).unwrap();
    for (world_param, domain_param) in
        [("gravity_x", "gravity_x"), ("gravity_y", "gravity"), ("gravity_z", "gravity_z"), ("speed", "speed"), ("reset", "reset")]
    {
        let source = result.wires.iter().find(|wire| wire.to_node == world.id && wire.to_port == world_param).unwrap();
        assert!(
            result.wires.iter().any(|wire| wire.from_node == source.from_node
                && wire.from_port == source.from_port
                && wire.to_node == fluid_group.id
                && wire.to_port == world_param),
            "{world_param} enters the fluid group"
        );
        assert!(
            outer.wires.iter().any(|wire| wire.from_node == outer_input.id
                && wire.from_port == world_param
                && wire.to_node == live.id
                && wire.to_port == world_param),
            "{world_param} enters Live Matter"
        );
        assert!(inner.interface.inputs.iter().any(|port| port.name == world_param && port.port_type == "Scalar(F32)"));
        assert!(
            inner.wires.iter().any(|wire| wire.from_node == inner_inputs[0].id
                && wire.from_port == world_param
                && wire.to_node == domain.id
                && wire.to_port == domain_param),
            "{world_param} reaches the domain"
        );
    }
    let flat = manifold_core::flatten::flatten_groups(result).expect("nested routing flattens");
    let flat_domain = flat.nodes.iter().find(|node| node.type_id == MATTER_DOMAIN_TYPE_ID).unwrap();
    for port in ["gravity_x", "gravity", "gravity_z", "speed", "reset"] {
        assert!(
            flat.wires.iter().any(|wire| wire.to_node == flat_domain.id && wire.to_port == port),
            "flattened graph drives the domain's {port}"
        );
    }
}

#[test]
fn scene_physics_add_fluid_rejects_a_template_with_two_liquid_domains() {
    let def = render_scene_graph(0, false);
    let mut template = liquid_template();
    let body = template.group.group.as_deref_mut().unwrap();
    let live = body.nodes.iter_mut().find(|node| node.node_id.as_str() == "live_matter").unwrap();
    let live_body = live.group.as_deref_mut().unwrap();
    let mut second = live_body.nodes[0].clone();
    second.id = 99;
    live_body.nodes.push(second);
    let (project, target) = project_with_graph(def.clone());
    assert!(insert(&project, &target, &def, template).is_err());
    assert_eq!(graph(&project, &target), &def);
}

#[test]
fn scene_physics_add_fluid_template_undo_reload() {
    let def = render_scene_graph(1, false);
    let (mut project, target) = project_with_graph(def.clone());
    let mut cmd = add(&mut project, &target, &def);
    let added = graph(&project, &target).clone();
    let group = added.nodes.iter().find(|node| node.handle.as_deref() == Some("Fluid 2 Graph")).unwrap();
    fn holds(nodes: &[EffectGraphNode], type_id: &str) -> bool {
        nodes.iter().any(|node| node.type_id == type_id || node.group.as_deref().is_some_and(|g| holds(&g.nodes, type_id)))
    }
    assert!(holds(&group.group.as_deref().unwrap().nodes, MATTER_DOMAIN_TYPE_ID));
    fn collect(nodes: &[EffectGraphNode], ids: &mut Vec<u32>) {
        for node in nodes {
            ids.push(node.id);
            if let Some(group) = node.group.as_deref() {
                collect(&group.nodes, ids);
            }
        }
    }
    let mut ids = Vec::new();
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
fn multi_output_reservation_rejects_later_occupied_slot_atomically() {
    let mut def = render_scene_graph(2, false);
    def.wires.push(wire(100, "object", 10, "object_3"));
    let (project, target) = project_with_graph(def.clone());
    let before = serde_json::to_value(&project).unwrap();
    let mut template = liquid_template();
    let body = template.group.group.as_deref_mut().unwrap();
    body.interface.outputs.push(InterfacePortDef { name: "object_1".into(), port_type: "SceneObject".into() });
    body.wires.push(wire(3, "object", 4, "object_1"));
    assert_eq!(
        insert(&project, &target, &def, template).err().as_deref(),
        Some("Render scene slot is occupied")
    );
    assert_eq!(serde_json::to_value(&project).unwrap(), before);
}
