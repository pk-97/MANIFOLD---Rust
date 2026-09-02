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
                node_id: manifold_core::NodeId::new("loop_phase"),
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
                node_id: manifold_core::NodeId::new("scene_array"),
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
                node_id: manifold_core::NodeId::new("loop_camera"),
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
        node_metadata: vec![],
        loop_camera_node_id: manifold_core::NodeId::new("loop_camera"),
        scene_array_node_id: manifold_core::NodeId::new("scene_array"),
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
        node_metadata: vec![],
        loop_camera_node_id: manifold_core::NodeId::new("loop_camera"),
        scene_array_node_id: manifold_core::NodeId::new("scene_array"),
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

/// INV-1/INV-2/INV-4 net against the REAL import shape: two object groups
/// wired into render_scene.object_0/object_1, lens between the orbit camera
/// and render/ao/dof/mb. Applies the loop and verifies:
///  - each group gained an `instances` interface input + group_input node +
///    inner wire to its scene_object,
///  - top-level `scene_array.out → group.instances` wires exist,
///  - `loop_camera.out → lens.camera` (the lens re-point, NOT render.camera),
///  - the old `orbit_camera.out → lens.camera` wire was dropped,
///  - loop nodes carry stable nodeIds.
#[test]
fn scene_loop_apply_splices_groups_and_repoints_lens() {
    use manifold_core::effect_graph_def::{
        EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef,
        GroupInterface, InterfacePortDef,
    };
    use manifold_editing::commands::graph::InstanceWiring;

    fn scene_object(id: u32, handle: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(handle),
            type_id: "node.scene_object".to_string(),
            handle: Some(handle.to_string()),
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
    fn group(id: u32, handle: &str, object_bind_id: u32) -> EffectGraphNode {
        let mut g = EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(handle),
            type_id: GROUP_TYPE_ID.to_string(),
            handle: Some(handle.to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        let out_id = object_bind_id + 1000;
        g.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: vec![InterfacePortDef { name: "object".to_string(), port_type: "Object".to_string() }],
                params: Vec::new(),
            },
            nodes: vec![
                scene_object(object_bind_id, &format!("{handle}_bind")),
                EffectGraphNode {
                    id: out_id,
                    node_id: manifold_core::NodeId::new(format!("{handle}_out")),
                    type_id: "system.group_output".to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
            ],
            wires: vec![EffectGraphWire {
                from_node: object_bind_id,
                from_port: "object".to_string(),
                to_node: out_id,
                to_port: "object".to_string(),
            }],
            tint: None,
        }));
        g
    }

    // ids: 0 camera(orbit), 1 lens, 2 render_scene, 10 group A (bind 11), 20 group B (bind 21).
    let def = EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("GroupSpliceTest".to_string()),
            display_name: "Scene".to_string(),
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
            scene_bounds: Some(([0.0, 0.0, 0.0], [1.0, 1.0, 10.0])),
        }),
        nodes: vec![
            EffectGraphNode {
                id: 0,
                node_id: manifold_core::NodeId::new("camera"),
                type_id: "node.orbit_camera".to_string(),
                handle: Some("camera".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 1,
                node_id: manifold_core::NodeId::new("lens"),
                type_id: "node.camera_lens".to_string(),
                handle: Some("lens".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            EffectGraphNode {
                id: 2,
                node_id: manifold_core::NodeId::new("render"),
                type_id: "node.render_scene".to_string(),
                handle: Some("render".to_string()),
                params: BTreeMap::new(),
                exposed_params: Default::default(),
                editor_pos: None,
                wgsl_source: None,
                title: None,
                output_formats: BTreeMap::new(),
                output_canvas_scales: BTreeMap::new(),
                group: None,
            },
            group(10, "Object A", 11),
            group(20, "Object B", 21),
        ],
        wires: vec![
            wire(0, "out", 1, "camera"),
            wire(1, "out", 2, "camera"),
            wire(10, "object", 2, "object_0"),
            wire(20, "object", 2, "object_1"),
        ],
    };
    // The panel's plan builder computes minted ids from max existing + 1..4.
    let max_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
    let (beat_id, array_id, cam_id, _fog_id) =
        (max_id + 1, max_id + 2, max_id + 3, max_id + 4);
    let plan = manifold_editing::commands::graph::SceneLoopPlan {
        new_nodes: vec![
            EffectGraphNode {
                id: beat_id,
                node_id: manifold_core::NodeId::new("loop_phase"),
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
                id: array_id,
                node_id: manifold_core::NodeId::new("scene_array"),
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
                id: cam_id,
                node_id: manifold_core::NodeId::new("loop_camera"),
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
            wire(beat_id, "out", cam_id, "phase"),
            wire(cam_id, "out", 1, "camera"), // → lens (D5 re-point)
        ],
        instance_wirings: vec![
            InstanceWiring { group_node_id: 10, scene_object_node_id: 11 },
            InstanceWiring { group_node_id: 20, scene_object_node_id: 21 },
        ],
        render_scene_node_id: 2,
        node_metadata: vec![],
        loop_camera_node_id: manifold_core::NodeId::new("loop_camera"),
        scene_array_node_id: manifold_core::NodeId::new("scene_array"),
    };

    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Grouped Scene",
        LayerType::Generator,
        PresetTypeId::from_string("GroupSpliceTest".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(def);
    }
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = manifold_editing::commands::graph::ApplySceneLoopCommand::new(
        target.clone(),
        vec![],
        plan,
        empty_def(),
    );
    cmd.execute(&mut project);

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present");

    // INV-1-camera: loop_camera drives lens.camera, and the old orbit wire is gone.
    assert!(
        graph
            .wires
            .iter()
            .any(|w| w.from_node == cam_id && w.to_node == 1 && w.to_port == "camera"),
        "loop_camera must re-point into lens.camera"
    );
    assert!(
        !graph
            .wires
            .iter()
            .any(|w| w.to_node == 1 && w.to_port == "camera" && w.from_node == 0),
        "old orbit_camera → lens.camera wire must be dropped"
    );
    assert!(
        !graph.wires.iter().any(|w| w.from_node == cam_id && w.to_node == 2),
        "loop_camera must NOT wire straight into render_scene when a lens exists"
    );

    // INV-2-splice: each object group gained an `instances` interface input,
    // a group_input node in the body, an inner wire to its scene_object, and
    // a top-level scene_array → group.instances wire.
    for (gid, bind) in [(10u32, 11u32), (20u32, 21u32)] {
        let group_node = graph
            .nodes
            .iter()
            .find(|n| n.id == gid)
            .expect("group survives apply");
        let body = group_node.group.as_deref().expect("group body survives");
        assert!(
            body.interface.inputs.iter().any(|p| p.name == "instances"),
            "group {gid} must gain an `instances` interface input"
        );
        let input_node = body
            .nodes
            .iter()
            .find(|n| n.type_id == GROUP_INPUT_TYPE_ID)
            .expect("group_input node minted in group body");
        let inner_wire = body
            .wires
            .iter()
            .find(|w| w.to_node == bind && w.to_port == "instances");
        assert!(
            inner_wire.is_some(),
            "group {gid}: group_input.instances → scene_object.instances inner wire missing"
        );
        if let Some(inner) = inner_wire {
            assert_eq!(
                inner.from_node, input_node.id,
                "group {gid}: inner instances wire must come from the group_input node"
            );
        }
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.to_node == gid && w.to_port == "instances" && w.from_node == array_id),
            "group {gid}: top-level scene_array.out → group.instances wire missing"
        );
    }

    // All three loop nodes present with stable nodeIds (INV-2).
    for expected in ["loop_phase", "scene_array", "loop_camera"] {
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.node_id.as_str() == expected),
            "loop node {expected} missing after apply"
        );
    }
}
