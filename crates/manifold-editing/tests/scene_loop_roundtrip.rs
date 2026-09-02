//! SCENE_LOOP_DESIGN.md INV-2/INV-4 round-trip gate:
//! apply → save → load → structural trace re-finds all loop nodes.
//!
//! Builds a minimal scene graph (one render_scene), applies the scene loop,
//! saves as V1, reloads, and asserts all loop nodes are present with stable
//! nodeIds (INV-2) and matching cell_size (INV-4).

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;

fn node(id: u32, type_id: &str, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(format!("n{id}")),
        type_id: type_id.to_string(),
        handle: Some(format!("n{id}")),
        params,
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
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

fn minimal_scene_def() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("TestScene".to_string()),
            display_name: "Test Scene".to_string(),
            category: "Geometry".to_string(),
            osc_prefix: "scene".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_bounds: None,
        }),
        nodes: vec![node(0, "node.render_scene", BTreeMap::new())],
        wires: vec![],
    }
}

fn empty_def() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: vec![],
        wires: vec![],
    }
}

#[test]
fn scene_loop_roundtrip_preserves_loop_nodes() {
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Test Scene",
        LayerType::Generator,
        PresetTypeId::from_string("TestScene".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(minimal_scene_def());
    }

    let plan = manifold_editing::commands::graph::SceneLoopPlan {
        new_nodes: vec![
            EffectGraphNode {
                id: 10,
                node_id: manifold_core::NodeId::new("loop_phase".to_string()),
                type_id: "node.beat_ramp".to_string(),
                handle: Some("loop_phase".to_string()),
                params: {
                    let mut p = BTreeMap::new();
                    p.insert("rate".to_string(), SerializedParamValue::Float { value: 0.125 });
                    p.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });
                    p
                },
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 11,
                node_id: manifold_core::NodeId::new("scene_array".to_string()),
                type_id: "node.scene_array".to_string(),
                handle: Some("scene_array".to_string()),
                params: {
                    let mut p = BTreeMap::new();
                    p.insert("count".to_string(), SerializedParamValue::Float { value: 3.0 });
                    p.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
                    p.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
                    p
                },
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 12,
                node_id: manifold_core::NodeId::new("loop_camera".to_string()),
                type_id: "node.loop_camera".to_string(),
                handle: Some("loop_camera".to_string()),
                params: {
                    let mut p = BTreeMap::new();
                    p.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
                    p.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
                    p
                },
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
        ],
        new_wires: vec![
            wire(10, "out", 12, "phase"),
            wire(12, "out", 0, "camera"),
        ],
        instance_wirings: vec![],
        render_scene_node_id: 0,
        loop_metadata: vec![],
        card_params: vec![],
        loop_camera_node_id: manifold_core::NodeId::new("loop_camera".to_string()),
        scene_array_node_id: manifold_core::NodeId::new("scene_array".to_string()),
    };

    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = manifold_editing::commands::graph::ApplySceneLoopCommand::new(
        target.clone(),
        vec![],
        plan,
        empty_def(),
    );
    cmd.execute(&mut project);

    // Save and reload.
    let path = std::env::temp_dir().join(format!(
        "manifold_scene_loop_rt_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v1");
    let reloaded = manifold_io::loader::load_project(&path).expect("load v1");
    let _ = std::fs::remove_file(&path);

    let reloaded_layer = reloaded
        .timeline
        .layers
        .iter()
        .find(|l| l.layer_type == LayerType::Generator)
        .expect("generator layer survived reload");
    let graph = reloaded_layer
        .generator_graph()
        .expect("graph override survived reload");

    // INV-2: all loop nodes present with stable nodeIds.
    let has_loop_phase = graph
        .nodes
        .iter()
        .any(|n| n.node_id.as_str() == "loop_phase" && n.type_id == "node.beat_ramp");
    let has_scene_array = graph
        .nodes
        .iter()
        .any(|n| n.node_id.as_str() == "scene_array" && n.type_id == "node.scene_array");
    let has_loop_camera = graph
        .nodes
        .iter()
        .any(|n| n.node_id.as_str() == "loop_camera" && n.type_id == "node.loop_camera");

    assert!(has_loop_phase, "INV-2: loop_phase not found after round-trip");
    assert!(has_scene_array, "INV-2: scene_array not found after round-trip");
    assert!(has_loop_camera, "INV-2: loop_camera not found after round-trip");

    // INV-4: cell_size matches between scene_array and loop_camera.
    let array_cell = graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "scene_array")
        .and_then(|n| match n.params.get("cell_size") {
            Some(SerializedParamValue::Float { value }) => Some(*value),
            _ => None,
        });
    let camera_cell = graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_camera")
        .and_then(|n| match n.params.get("cell_size") {
            Some(SerializedParamValue::Float { value }) => Some(*value),
            _ => None,
        });
    assert_eq!(
        array_cell, camera_cell,
        "INV-4: cell_size mismatch between scene_array and loop_camera"
    );
}

#[test]
fn scene_loop_apply_rejects_multi_scene() {
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Multi Scene",
        LayerType::Generator,
        PresetTypeId::from_string("MultiScene".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![
                node(0, "node.render_scene", BTreeMap::new()),
                node(1, "node.render_scene", BTreeMap::new()),
            ],
            wires: vec![],
        });
    }

    let plan = manifold_editing::commands::graph::SceneLoopPlan {
        new_nodes: vec![],
        new_wires: vec![],
        instance_wirings: vec![],
        render_scene_node_id: 0,
        loop_metadata: vec![],
        card_params: vec![],
        loop_camera_node_id: manifold_core::NodeId::new("loop_camera".to_string()),
        scene_array_node_id: manifold_core::NodeId::new("scene_array".to_string()),
    };

    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = manifold_editing::commands::graph::ApplySceneLoopCommand::new(
        target,
        vec![],
        plan,
        empty_def(),
    );
    cmd.execute(&mut project);

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present");
    // INV-1: exactly 2 render_scene nodes, no loop nodes added.
    assert_eq!(graph.nodes.len(), 2, "INV-1: loop nodes should not be added to multi-scene graph");
}
