//! SCENE_MODIFIER_FRAMEWORK P1 command-level gate (the INV-2/INV-4
//! round-trip pattern SCENE_LOOP used, now against the GENERIC
//! ApplySceneModifierCommand/RemoveSceneModifierCommand — D1/D6):
//! apply → save → load → structural trace re-finds all loop nodes.
//!
//! The plan here is HAND-BUILT (manifold-editing has no renderer
//! dependency) — the renderer-side descriptor builder that production
//! dispatches is gated end to end in manifold-renderer's
//! `scene_loop_roundtrip_gate.rs` / `scene_loop_e2e_import.rs`.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    GROUP_INPUT_TYPE_ID, GROUP_TYPE_ID, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    GroupDef, GroupInterface, InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::scene_modifier::{
    EnablePlan, GroupSplice, PlanTraceNode, PortRepoint, SceneModifierPlan, ToggleDecl,
};
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::ApplySceneModifierCommand;

fn node(id: u32, node_id: &str, type_id: &str, params: BTreeMap<String, SerializedParamValue>) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: manifold_core::NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(node_id.to_string()),
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

/// The loop kind's trace signature, hand-mirrored from the renderer-side
/// descriptor (the descriptor itself is renderer-crate; the command only
/// needs the data).
fn loop_trace() -> Vec<PlanTraceNode> {
    vec![
        PlanTraceNode { type_id: "node.beat_ramp".into(), node_id: "loop_phase".into(), required: true },
        PlanTraceNode { type_id: "node.scene_array".into(), node_id: "scene_array".into(), required: true },
        PlanTraceNode { type_id: "node.loop_camera".into(), node_id: "loop_camera".into(), required: true },
        PlanTraceNode { type_id: "node.camera_switch".into(), node_id: "loop_cam_switch".into(), required: false },
    ]
}

/// Hand-built loop plan on the minimal scene (no lens): the camera path
/// re-points INTO render_scene.camera through the loop_cam_switch (D5
/// Switch). Minted ids start at 10.
fn minimal_loop_plan(render_scene_doc: u32) -> SceneModifierPlan {
    let switch_id = 13;
    let mut switch_params = BTreeMap::new();
    switch_params.insert("select".to_string(), SerializedParamValue::Enum { value: 1 }); // B
    let switch = node(switch_id, "loop_cam_switch", "node.camera_switch", switch_params);

    let mut phase_params = BTreeMap::new();
    phase_params.insert("rate".to_string(), SerializedParamValue::Float { value: 0.125 });
    phase_params.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });

    let mut array_params = BTreeMap::new();
    array_params.insert("count".to_string(), SerializedParamValue::Float { value: 3.0 });
    array_params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    array_params.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });

    let mut camera_params = BTreeMap::new();
    camera_params.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
    camera_params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });

    SceneModifierPlan {
        kind_id: "scene_loop".to_string(),
        display_name: "Scene Loop".to_string(),
        trace: loop_trace(),
        new_nodes: vec![
            node(10, "loop_phase", "node.beat_ramp", phase_params),
            node(11, "scene_array", "node.scene_array", array_params),
            node(12, "loop_camera", "node.loop_camera", camera_params),
        ],
        new_wires: vec![wire(10, "out", 12, "phase")],
        group_splices: vec![],
        repoints: vec![PortRepoint {
            target_node_id: render_scene_doc,
            target_port: "camera".to_string(),
            new_producer_doc_id: switch_id,
            restore_types: &["node.orbit_camera", "node.free_camera", "node.look_at_camera"],
        }],
        exposures: vec![],
        enable: EnablePlan {
            toggle: ToggleDecl::NodeParam {
                node_doc_hint: manifold_core::NodeId::new("loop_cam_switch"),
                param: "select".to_string(),
                on: 1.0,
                off: 0.0,
            },
            extra_nodes: vec![switch],
            extra_wires: vec![
                wire(12, "out", switch_id, "b"),
                wire(switch_id, "out", render_scene_doc, "camera"),
            ],
        },
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
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_bounds: None,
        }),
        nodes: vec![node(0, "render", "node.render_scene", BTreeMap::new())],
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

    let plan = minimal_loop_plan(0);

    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = ApplySceneModifierCommand::new(target.clone(), Vec::new(), plan, empty_def());
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

    // INV-2: all loop nodes present with stable nodeIds (incl. the D5
    // camera switch).
    for (node_id, type_id) in [
        ("loop_phase", "node.beat_ramp"),
        ("scene_array", "node.scene_array"),
        ("loop_camera", "node.loop_camera"),
        ("loop_cam_switch", "node.camera_switch"),
    ] {
        assert!(
            graph.nodes.iter().any(|n| n.node_id.as_str() == node_id && n.type_id == type_id),
            "INV-2: {node_id} not found after round-trip"
        );
    }

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
                node(0, "render_a", "node.render_scene", BTreeMap::new()),
                node(1, "render_b", "node.render_scene", BTreeMap::new()),
            ],
            wires: vec![],
        });
    }

    let plan = minimal_loop_plan(0);

    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = ApplySceneModifierCommand::new(target, Vec::new(), plan, empty_def());
    cmd.execute(&mut project);

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present");
    // INV-M6: exactly 2 render_scene nodes, no loop nodes added.
    assert_eq!(graph.nodes.len(), 2, "INV-M6: modifier nodes must not be added to a multi-scene graph");
}

fn scene_object_node(id: u32, handle: &str) -> EffectGraphNode {
    node(id, handle, "node.scene_object", BTreeMap::new())
}

fn object_group(id: u32, handle: &str, object_bind_id: u32) -> EffectGraphNode {
    let mut g = node(id, handle, GROUP_TYPE_ID, BTreeMap::new());
    let out_id = object_bind_id + 1000;
    g.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef { name: "object".to_string(), port_type: "Object".to_string() }],
            params: Vec::new(),
        },
        nodes: vec![
            scene_object_node(object_bind_id, &format!("{handle}_bind")),
            node(out_id, &format!("{handle}_out"), "system.group_output", BTreeMap::new()),
        ],
        wires: vec![wire(object_bind_id, "object", out_id, "object")],
        tint: None,
    }));
    g
}

/// The two-object-group scene the splice tests share: ids 0 camera(orbit),
/// 1 lens, 2 render_scene, 10 group A (bind 11), 20 group B (bind 21).
fn grouped_scene_def() -> EffectGraphDef {
    EffectGraphDef {
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
                layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
            scene_bounds: Some(([0.0, 0.0, 0.0], [1.0, 1.0, 10.0])),
        }),
        nodes: vec![
            node(0, "camera", "node.orbit_camera", BTreeMap::new()),
            node(1, "lens", "node.camera_lens", BTreeMap::new()),
            node(2, "render", "node.render_scene", BTreeMap::new()),
            object_group(10, "Object A", 11),
            object_group(20, "Object B", 21),
        ],
        wires: vec![
            wire(0, "out", 1, "camera"),
            wire(1, "out", 2, "camera"),
            wire(10, "object", 2, "object_0"),
            wire(20, "object", 2, "object_1"),
        ],
    }
}

/// Hand-built loop plan on the grouped scene (minted ids 30..33): splices
/// WITH take-over consent (replace_existing: true — the D6 loop semantics),
/// lens camera repoint, switch enable. Shared so the take-over tests build
/// on the exact applied state the loop leaves behind.
fn grouped_loop_plan() -> SceneModifierPlan {
    let switch_id = 33;
    let mut switch_params = BTreeMap::new();
    switch_params.insert("select".to_string(), SerializedParamValue::Enum { value: 1 });
    let mut phase_params = BTreeMap::new();
    phase_params.insert("rate".to_string(), SerializedParamValue::Float { value: 0.125 });
    phase_params.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });
    let mut array_params = BTreeMap::new();
    array_params.insert("count".to_string(), SerializedParamValue::Float { value: 3.0 });
    array_params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    array_params.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
    let mut camera_params = BTreeMap::new();
    camera_params.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
    camera_params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });

    SceneModifierPlan {
        kind_id: "scene_loop".to_string(),
        display_name: "Scene Loop".to_string(),
        trace: loop_trace(),
        new_nodes: vec![
            node(30, "loop_phase", "node.beat_ramp", phase_params),
            node(31, "scene_array", "node.scene_array", array_params),
            node(32, "loop_camera", "node.loop_camera", camera_params),
        ],
        new_wires: vec![wire(30, "out", 32, "phase")],
        group_splices: vec![
            GroupSplice {
                group_node_id: 10,
                inner_node_type: "node.scene_object",
                inner_port: "instances",
                source_doc_id: 31,
                source_port: "out".to_string(),
                replace_existing: true,
            },
            GroupSplice {
                group_node_id: 20,
                inner_node_type: "node.scene_object",
                inner_port: "instances",
                source_doc_id: 31,
                source_port: "out".to_string(),
                replace_existing: true,
            },
        ],
        repoints: vec![PortRepoint {
            target_node_id: 1,
            target_port: "camera".to_string(),
            new_producer_doc_id: switch_id,
            restore_types: &["node.orbit_camera", "node.free_camera", "node.look_at_camera"],
        }],
        exposures: vec![],
        enable: EnablePlan {
            toggle: ToggleDecl::NodeParam {
                node_doc_hint: manifold_core::NodeId::new("loop_cam_switch"),
                param: "select".to_string(),
                on: 1.0,
                off: 0.0,
            },
            extra_nodes: vec![node(switch_id, "loop_cam_switch", "node.camera_switch", switch_params)],
            extra_wires: vec![
                wire(0, "out", switch_id, "a"),
                wire(32, "out", switch_id, "b"),
                wire(switch_id, "out", 1, "camera"),
            ],
        },
    }
}

/// A second modifier's plan splicing group 10's `instances` from
/// `source_doc_id` — the kind-#3 (mirror-shaped) splice. `replace_existing`
/// selects which INV-MR8 arm applies.
fn takeover_plan(source_doc_id: u32, replace_existing: bool) -> SceneModifierPlan {
    SceneModifierPlan {
        kind_id: "modifier_b".to_string(),
        display_name: "Modifier B".to_string(),
        trace: vec![],
        new_nodes: vec![node(
            source_doc_id,
            "modifier_b_src",
            "node.scene_array",
            BTreeMap::new(),
        )],
        new_wires: vec![],
        group_splices: vec![GroupSplice {
            group_node_id: 10,
            inner_node_type: "node.scene_object",
            inner_port: "instances",
            source_doc_id,
            source_port: "out".to_string(),
            replace_existing,
        }],
        repoints: vec![],
        exposures: vec![],
        enable: EnablePlan {
            toggle: ToggleDecl::ValueAtom {
                node_id: manifold_core::NodeId::new("modifier_b_enable"),
            },
            extra_nodes: vec![],
            extra_wires: vec![],
        },
    }
}

fn grouped_project() -> (Project, usize) {
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Grouped Scene",
        LayerType::Generator,
        PresetTypeId::from_string("GroupSpliceTest".to_string()),
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(grouped_scene_def());
    (project, idx)
}

fn apply_on_grouped(project: &mut Project, idx: usize, plan: SceneModifierPlan) {
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let mut cmd = ApplySceneModifierCommand::new(target, Vec::new(), plan, empty_def());
    cmd.execute(project);
}

/// INV-M6/INV-2/INV-4 net against the REAL import shape: two object groups
/// wired into render_scene.object_0/object_1, lens between the orbit camera
/// and render. Applies the loop (hand-built plan with splices + the switch
/// repoint) and verifies:
///  - each group gained an `instances` interface input + group_input node +
///    inner wire to its scene_object,
///  - top-level `scene_array.out → group.instances` wires exist,
///  - the camera path runs orbit → loop_cam_switch.a / loop_camera → b /
///    switch.out → lens.camera (the D5 Switch shape), and the old
///    `orbit_camera.out → lens.camera` wire was dropped,
///  - loop nodes carry stable nodeIds (INV-2).
#[test]
fn scene_loop_apply_splices_groups_and_repoints_lens() {
    let def = grouped_scene_def();

    // Hand-built plan on the grouped scene: minted ids 30..33, camera
    // target = the lens (1), previous camera producer = the orbit (0).
    let switch_id = 33;
    let plan = grouped_loop_plan();

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
    let mut cmd = ApplySceneModifierCommand::new(target.clone(), Vec::new(), plan, empty_def());
    cmd.execute(&mut project);

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present");

    // INV-1-camera (D5 Switch): loop_camera → switch.b, switch.out →
    // lens.camera, and the old orbit_camera → lens.camera wire dropped.
    assert!(
        graph
            .wires
            .iter()
            .any(|w| w.from_node == 32 && w.to_node == switch_id && w.to_port == "b"),
        "loop_camera must feed the switch's b input"
    );
    assert!(
        graph
            .wires
            .iter()
            .any(|w| w.from_node == switch_id && w.to_node == 1 && w.to_port == "camera"),
        "switch.out must re-point into lens.camera"
    );
    assert!(
        !graph
            .wires
            .iter()
            .any(|w| w.to_node == 1 && w.to_port == "camera" && w.from_node == 0),
        "old orbit_camera → lens.camera wire must be dropped"
    );
    assert!(
        graph
            .wires
            .iter()
            .any(|w| w.from_node == 0 && w.to_node == switch_id && w.to_port == "a"),
        "the previous camera producer must feed the switch's a input"
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
                .any(|w| w.to_node == gid && w.to_port == "instances" && w.from_node == 31),
            "group {gid}: top-level scene_array.out → group.instances wire missing"
        );
    }

    // All four minted nodes present with stable nodeIds (INV-2).
    for expected in ["loop_phase", "scene_array", "loop_camera", "loop_cam_switch"] {
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.node_id.as_str() == expected),
            "modifier node {expected} missing after apply"
        );
    }
}

/// INV-MR8 replace arm (SCENE_MIRROR_DESIGN section 3.5): a second splicer
/// WITH replace_existing takes the port over — the previous owner's wire
/// to (group, instances) is dropped, the new source's lands, and the
/// interface input + group_input are not duplicated. Group 20 keeps its
/// owner: the take-over is per (group, port).
#[test]
fn scene_modifier_splice_replace_existing_takes_over_port() {
    let (mut project, idx) = grouped_project();
    apply_on_grouped(&mut project, idx, grouped_loop_plan());

    // The kind-#3 (mirror-shaped) splicer takes group 10's port.
    apply_on_grouped(&mut project, idx, takeover_plan(40, true));

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present");

    let feeders: Vec<u32> = graph
        .wires
        .iter()
        .filter(|w| w.to_node == 10 && w.to_port == "instances")
        .map(|w| w.from_node)
        .collect();
    assert_eq!(
        feeders,
        vec![40],
        "exactly one wire to (group 10, instances), from the new owner"
    );
    assert!(
        !graph
            .wires
            .iter()
            .any(|w| w.to_node == 10 && w.to_port == "instances" && w.from_node == 31),
        "the displaced owner's wire to group 10 must be dropped"
    );
    assert!(
        graph
            .wires
            .iter()
            .any(|w| w.to_node == 20 && w.to_port == "instances" && w.from_node == 31),
        "group 20 keeps its owner — the take-over is per (group, port)"
    );

    let group = graph.nodes.iter().find(|n| n.id == 10).expect("group 10 present");
    let body = group.group.as_deref().expect("group 10 body present");
    assert_eq!(
        body.interface.inputs.iter().filter(|p| p.name == "instances").count(),
        1,
        "the take-over must not duplicate the interface input"
    );
    assert_eq!(
        body.nodes.iter().filter(|n| n.type_id == GROUP_INPUT_TYPE_ID).count(),
        1,
        "the take-over must not duplicate the group_input node"
    );
    assert!(
        graph.nodes.iter().any(|n| n.id == 31),
        "the displaced owner node stays in-graph inert (D6)"
    );
}

/// INV-MR8 fail-loud arm (SCENE_MIRROR_DESIGN section 3.5): a splice
/// WITHOUT replace_existing onto an already-spliced port refuses the WHOLE
/// apply — no nodes, no wire. The old conflated behaviour added the nodes
/// and silently skipped the wire, leaving the new kind believing it owned a
/// port it never wired.
#[test]
fn scene_modifier_splice_without_takeover_fails_loud() {
    let (mut project, idx) = grouped_project();
    apply_on_grouped(&mut project, idx, grouped_loop_plan());
    let nodes_before = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present")
        .nodes
        .len();

    apply_on_grouped(&mut project, idx, takeover_plan(50, false));

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph present");
    assert_eq!(
        graph.nodes.len(),
        nodes_before,
        "fail-loud: the refused apply must add no nodes"
    );
    assert!(
        !graph.nodes.iter().any(|n| n.id == 50),
        "the refused splicer's node is absent"
    );
    assert!(
        !graph.wires.iter().any(|w| w.from_node == 50),
        "no wire from the refused splicer"
    );
    assert!(
        graph
            .wires
            .iter()
            .any(|w| w.from_node == 31 && w.to_node == 10 && w.to_port == "instances"),
        "the existing owner's wire survives untouched"
    );
}
