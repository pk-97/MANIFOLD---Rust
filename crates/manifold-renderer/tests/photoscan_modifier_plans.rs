//! Structural acceptance coverage for the photo-scan scene modifier recipes.
//!
//! This deliberately uses the production GLB importer and editing commands;
//! the optional proof directory lets the lead render the exact applied graph
//! without making renderer production code depend on the editing crate.

use std::collections::BTreeSet;
use std::path::Path;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GroupDef, GroupInterface, InterfacePortDef,
    SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{ApplySceneModifierCommand, RemoveSceneModifierCommand};
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier::{build_plan, descriptor_for};
use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const KINDS: &[&str] = &["elastic_sculpture", "surface_peel", "vortex_fragments"];

fn render_scene_id(def: &EffectGraphDef) -> u32 {
    def.nodes
        .iter()
        .find(|node| node.type_id == RENDER_SCENE_TYPE_ID)
        .map(|node| node.id)
        .expect("production mushroom import must contain render_scene")
}

fn proof_json(dir: &Path, name: &str, def: &EffectGraphDef) {
    std::fs::create_dir_all(dir).expect("proof directory must be writable");
    let path = dir.join(name);
    let json = serde_json::to_string_pretty(def).expect("graph must serialize");
    std::fs::write(&path, json).expect("proof graph must be writable");
}

fn assert_unique_document_ids(nodes: &[EffectGraphNode], stable: &mut BTreeSet<String>) {
    let mut local = BTreeSet::new();
    for node in nodes {
        assert!(local.insert(node.id), "document id {} must be unique within graph scope", node.id);
        if !node.node_id.is_empty() {
            assert!(stable.insert(node.node_id.as_str().to_string()), "stable node id {} must be unique", node.node_id);
        }
        if let Some(group) = node.group.as_deref() {
            assert_unique_document_ids(&group.nodes, stable);
        }
    }
}

fn node(id: u32, node_id: &str, type_id: &str, params: &[(&str, SerializedParamValue)]) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(node_id.to_string()),
        params: params.iter().map(|(name, value)| ((*name).to_string(), value.clone())).collect(),
        exposed_params: Default::default(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: Default::default(),
        output_canvas_scales: Default::default(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire { from_node, from_port: from_port.to_string(), to_node, to_port: to_port.to_string() }
}

fn nested_scene(template: &EffectGraphDef) -> EffectGraphDef {
    fn object_group(base: u32, name: &str) -> EffectGraphNode {
        let source = node(base + 1, &format!("{name}_mesh"), "node.cube_mesh", &[]);
        let transform = node(base + 2, &format!("{name}_transform"), "node.transform_3d", &[
            ("pos_x", SerializedParamValue::Float { value: base as f32 * 0.01 }),
            ("pos_y", SerializedParamValue::Float { value: 0.0 }),
            ("pos_z", SerializedParamValue::Float { value: 0.0 }),
        ]);
        let material_a = node(base + 3, &format!("{name}_material_a"), "node.pbr_material", &[]);
        let material_b = node(base + 4, &format!("{name}_material_b"), "node.phong_material", &[]);
        let object = node(base + 5, &format!("{name}_object"), "node.scene_object", &[]);
        let output = node(base + 6, &format!("{name}_output"), "system.group_output", &[]);
        let group = GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: vec![InterfacePortDef { name: "mesh_object".into(), port_type: "Object".into() }],
                params: Vec::new(),
            },
            nodes: vec![source, transform, material_a, material_b, object, output],
            wires: vec![
                wire(base + 1, "out", base + 5, "vertices"),
                wire(base + 2, "out", base + 5, "transform"),
                wire(base + 3, "out", base + 5, "material"),
                wire(base + 4, "out", base + 5, "base_color_map"),
                wire(base + 5, "object", base + 6, "mesh_object"),
            ],
            tint: None,
        };
        let mut wrapper = node(base, name, "group", &[]);
        wrapper.group = Some(Box::new(group));
        wrapper
    }
    EffectGraphDef {
        version: 2,
        name: Some("Synthetic Photo Scan".into()),
        description: None,
        preset_metadata: template.preset_metadata.clone().map(|mut metadata| { metadata.scene_bounds = Some(([-2.0; 3], [2.0; 3])); metadata }),
        nodes: vec![
            node(1, "render", RENDER_SCENE_TYPE_ID, &[]),
            object_group(10, "left_object"),
            object_group(20, "right_object"),
        ],
        wires: vec![wire(10, "mesh_object", 1, "object_0"), wire(20, "mesh_object", 1, "object_1")],
    }
}

fn project_with_graph(def: EffectGraphDef) -> (Project, usize, manifold_core::LayerId) {
    let mut project = Project::default();
    let index = project.timeline.add_layer("Photo Scan", LayerType::Generator, PresetTypeId::from_string("SceneStarter".into()));
    project.timeline.layers[index].gen_params_or_init().graph = Some(def);
    let id = project.timeline.layers[index].layer_id.clone();
    (project, index, id)
}

fn empty_catalog() -> EffectGraphDef {
    EffectGraphDef { version: 1, name: None, description: None, preset_metadata: None, nodes: Vec::new(), wires: Vec::new() }
}

#[test]
fn photoscan_modifier_real_import() {
    let fixture = Path::new(MUSHROOM_FIXTURE);
    assert!(fixture.is_absolute(), "fixture path must remain an absolute production path");
    let (baseline, _report) = assemble_import_graph(fixture)
        .unwrap_or_else(|error| panic!("assemble_import_graph({MUSHROOM_FIXTURE}) failed: {error}"));
    let scene_id = render_scene_id(&baseline);

    let proof_dir = std::env::var_os("MANIFOLD_PHOTOSCAN_PROOF_DIR").map(std::path::PathBuf::from);
    if let Some(dir) = &proof_dir {
        proof_json(dir, "baseline.json", &baseline);
    }

    for &kind in KINDS {
        let reloaded: EffectGraphDef = serde_json::from_str(
            &serde_json::to_string(&baseline).expect("baseline must serialize"),
        )
        .expect("baseline graph must reload");
        let descriptor = descriptor_for(kind).expect("photo-scan descriptor must be registered");
        assert!(
            (descriptor.applicable)(&reloaded, scene_id),
            "{kind} must apply to the production mushroom import"
        );
        let plan = build_plan(kind, &reloaded, scene_id).expect("photo-scan plan must build");
        assert!(!plan.mesh_stages.is_empty(), "{kind} must target imported mesh stages");
        assert!(!plan.shared_params.is_empty(), "{kind} must fan controls into stage atoms");

        let mut project = Project::default();
        let layer_index = project.timeline.add_layer(
            "Photo Scan",
            LayerType::Generator,
            PresetTypeId::from_string("SceneStarter".to_string()),
        );
        project.timeline.layers[layer_index].gen_params_or_init().graph = Some(reloaded.clone());
        let layer_id = project.timeline.layers[layer_index].layer_id.clone();
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let mut apply = ApplySceneModifierCommand::new(
            target.clone(),
            Vec::new(),
            plan,
            EffectGraphDef {
                version: 1,
                name: None,
                description: None,
                preset_metadata: None,
                nodes: Vec::new(),
                wires: Vec::new(),
            },
        );
        apply.execute(&mut project);
        let applied = project.timeline.layers[layer_index]
            .generator_graph()
            .expect("apply must retain graph")
            .clone();
        assert_unique_document_ids(&applied.nodes, &mut BTreeSet::new());
        manifold_renderer::preset_runtime::PresetRuntime::from_def(
            applied.clone(),
            &manifold_renderer::node_graph::PrimitiveRegistry::with_builtin(),
            None,
        )
        .unwrap_or_else(|error| panic!("{kind} applied graph must compile: {error}"));
        assert!(
            applied
                .nodes
                .iter()
                .any(|node| node.node_id.as_str().starts_with(&format!("photoscan/{kind}/"))),
            "{kind} must stamp its top-level controls"
        );
        if let Some(dir) = &proof_dir {
            proof_json(dir, &format!("{kind}.json"), &applied);
        }

        let applied_reloaded: EffectGraphDef = serde_json::from_str(
            &serde_json::to_string(&applied).expect("applied graph must serialize"),
        )
        .expect("applied graph must reload");
        project.timeline.layers[layer_index].gen_params_or_init().graph = Some(applied_reloaded.clone());
        let remove_plan = build_plan(kind, &applied_reloaded, scene_id).expect("applied plan must re-derive for remove");
        let mut remove = RemoveSceneModifierCommand::new(target, Vec::new(), remove_plan);
        remove.execute(&mut project);
        let restored = project.timeline.layers[layer_index]
            .generator_graph()
            .expect("remove must retain graph")
            .clone();
        assert_eq!(restored, reloaded, "{kind} apply/remove must restore the reloaded graph");
    }
}

#[test]
fn photoscan_modifier_direct_root_object_fixture() {
    // Flattening the production import gives the other supported attachment
    // shape: scene_object, transform_3d and mesh source are siblings at the
    // scene level. This catches stage/controller document-id collisions.
    let (imported, _report) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE)).expect("mushroom import");
    let direct = manifold_core::flatten::flatten_groups(&imported).expect("import must flatten");
    let scene_id = render_scene_id(&direct);
    for &kind in KINDS {
        let plan = build_plan(kind, &direct, scene_id).expect("direct-root plan must build");
        let mut project = Project::default();
        let layer_index = project.timeline.add_layer("Photo Scan Flat", LayerType::Generator, PresetTypeId::from_string("SceneStarter".to_string()));
        project.timeline.layers[layer_index].gen_params_or_init().graph = Some(direct.clone());
        let layer_id = project.timeline.layers[layer_index].layer_id.clone();
        let mut apply = ApplySceneModifierCommand::new(
            manifold_core::GraphTarget::Generator(layer_id), Vec::new(), plan,
            EffectGraphDef { version: 1, name: None, description: None, preset_metadata: None, nodes: Vec::new(), wires: Vec::new() },
        );
        apply.execute(&mut project);
        let applied = project.timeline.layers[layer_index].generator_graph().expect("direct-root apply");
        assert_unique_document_ids(&applied.nodes, &mut BTreeSet::new());
    }
}

#[test]
fn photoscan_modifier_synthetic_stack_roundtrip_and_middle_remove() {
    let (imported, _report) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE)).expect("mushroom import");
    let baseline = nested_scene(&imported);
    let scene_id = render_scene_id(&baseline);
    let (mut project, index, layer_id) = project_with_graph(baseline.clone());
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());

    for &kind in KINDS {
        let current = project.timeline.layers[index].generator_graph().expect("synthetic graph").clone();
        manifold_core::flatten::flatten_groups(&current)
            .unwrap_or_else(|error| panic!("graph before {kind} must flatten: {error}"));
        let plan = build_plan(kind, &current, scene_id)
            .unwrap_or_else(|| panic!("synthetic {kind} plan"));
        assert_eq!(plan.mesh_stages.len(), 2, "{kind} must attach to both nested objects");
        let mut apply = ApplySceneModifierCommand::new(target.clone(), Vec::new(), plan, empty_catalog());
        apply.execute(&mut project);
    }

    let stacked = project.timeline.layers[index].generator_graph().expect("stacked graph").clone();
    let stacked_json = serde_json::to_string(&stacked).expect("stacked graph serializes");
    project.timeline.layers[index].gen_params_or_init().graph = Some(serde_json::from_str(&stacked_json).expect("stacked graph reloads"));

    let surface_graph = project.timeline.layers[index].generator_graph().expect("surface stack").clone();
    let surface_plan = build_plan("surface_peel", &surface_graph, scene_id).expect("surface removal plan");
    let mut remove_surface = RemoveSceneModifierCommand::new(target.clone(), Vec::new(), surface_plan);
    remove_surface.execute(&mut project);
    let middle_removed = project.timeline.layers[index].generator_graph().expect("middle removal graph").clone();
    assert!(middle_removed.nodes.iter().all(|n| !n.node_id.as_str().starts_with("photoscan/surface_peel/")), "middle modifier controls must be removed");
    assert!(middle_removed.nodes.iter().any(|n| n.node_id.as_str().starts_with("photoscan/elastic_sculpture/")), "first modifier must remain");
    assert!(middle_removed.nodes.iter().any(|n| n.node_id.as_str().starts_with("photoscan/vortex_fragments/")), "last modifier must remain");

    for &kind in ["elastic_sculpture", "vortex_fragments"].iter() {
        let current = project.timeline.layers[index].generator_graph().expect("remaining stack").clone();
        let plan = build_plan(kind, &current, scene_id).expect("remaining removal plan");
        let mut remove = RemoveSceneModifierCommand::new(target.clone(), Vec::new(), plan);
        remove.execute(&mut project);
    }
    let restored = project.timeline.layers[index].generator_graph().expect("restored synthetic graph");
    assert_eq!(restored, &baseline, "apply three, remove middle, remove all must restore graph");
}

#[test]
fn photoscan_modifier_controls_fan_out_and_reject_unsupported_object() {
    let (imported, _report) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE)).expect("mushroom import");
    let baseline = nested_scene(&imported);
    let scene_id = render_scene_id(&baseline);
    for &kind in KINDS {
        let plan = build_plan(kind, &baseline, scene_id).expect("synthetic plan");
        let control_ids: BTreeSet<&str> = plan.new_nodes.iter().map(|n| n.node_id.as_str()).collect();
        for control in &control_ids {
            let fanout = plan.shared_params.iter().filter(|link| match &link.source {
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => node_id.as_str() == *control && param == "value",
                _ => false,
            }).count();
            assert!(fanout > 0, "{kind} control {control} must drive a real stage atom");
        }
    }

    let mut unsupported = baseline.clone();
    unsupported.nodes.push({
        let mut custom = node(99, "unsupported_object", "node.value", &[]);
        custom.handle = Some("unsupported_object".into());
        custom
    });
    unsupported.wires.push(wire(99, "out", scene_id, "object_2"));
    let descriptor = descriptor_for("elastic_sculpture").expect("descriptor");
    assert!(!(descriptor.applicable)(&unsupported, scene_id), "unsupported connected object input must refuse applicability");
    assert!(build_plan("elastic_sculpture", &unsupported, scene_id).is_none(), "unsupported connected object input must refuse planning");

    let mut renamed = baseline.clone();
    if let Some(group) = renamed.nodes.iter_mut().find(|n| n.node_id.as_str() == "left_object").and_then(|n| n.group.as_mut()) {
        if let Some(interface) = group.interface.outputs.iter_mut().find(|port| port.name == "mesh_object") {
            interface.name = "custom_object".into();
        }
        if let Some(output) = group.nodes.iter_mut().find(|n| n.type_id == "system.group_output") {
            output.handle = Some("custom_output".into());
        }
        if let Some(wire) = group.wires.iter_mut().find(|wire| wire.to_port == "mesh_object") {
            wire.to_port = "custom_object".into();
        }
    }
    if let Some(wire) = renamed.wires.iter_mut().find(|wire| wire.from_node == 10) {
        wire.from_port = "custom_object".into();
    }
    assert!(build_plan("elastic_sculpture", &renamed, scene_id).is_some(), "flattened custom group output names must still resolve");
}

#[test]
fn photoscan_modifier_remove_survives_deleted_object_and_bounds() {
    let (imported, _report) = assemble_import_graph(Path::new(MUSHROOM_FIXTURE)).expect("mushroom import");
    let baseline = nested_scene(&imported);
    let scene_id = render_scene_id(&baseline);
    let (mut project, index, layer_id) = project_with_graph(baseline.clone());
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let plan = build_plan("surface_peel", &baseline, scene_id).expect("surface plan");
    let mut apply = ApplySceneModifierCommand::new(target.clone(), Vec::new(), plan, empty_catalog());
    apply.execute(&mut project);
    let graph = project.timeline.layers[index].generator_graph().expect("applied graph");
    let mut changed = graph.clone();
    changed.nodes.retain(|node| node.node_id.as_str() != "right_object");
    changed.wires.retain(|wire| !(wire.from_node == 20 || wire.to_port == "object_1"));
    if let Some(metadata) = changed.preset_metadata.as_mut() {
        metadata.scene_bounds = None;
    }
    project.timeline.layers[index].gen_params_or_init().graph = Some(changed.clone());
    let remove_plan = build_plan("surface_peel", &changed, scene_id).expect("orphan stage removal must not need bounds or live target");
    let mut remove = RemoveSceneModifierCommand::new(target, Vec::new(), remove_plan);
    remove.execute(&mut project);
    let restored = project.timeline.layers[index].generator_graph().expect("removed graph");
    assert!(restored.nodes.iter().all(|node| !node.node_id.as_str().starts_with("photoscan/surface_peel/")), "all owned stage/control nodes must be removed");
    assert!(restored.preset_metadata.as_ref().is_some_and(|metadata| metadata.scene_bounds.is_none()), "removed graph must retain deleted bounds state");
}
