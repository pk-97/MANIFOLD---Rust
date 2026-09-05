//! SCENE_MIRROR_DESIGN P2 round-trip gate (the `scene_loop_roundtrip_gate.rs`
//! pattern for the mirror kind): apply loop → mirror through the REAL
//! descriptor plan builders + the REAL generic command → save V1 → reload
//! → run the load-time migrations + reconcile → assert the "Scene Mirror"
//! section rows are EXACTLY the performer whitelist (Enabled, Axis,
//! Plane Offset), values intact, a performer edit survives, and the
//! D6-ordered remove (mirror then loop) after the reload restores the
//! exact original graph.
//!
//! This is the BUG-pvbu (scene-loop-panel-params-dropped-on-reload) class
//! gate for kind #3: the stamped rows must survive the save/reload/
//! reconcile cycle and stay writable.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::{
    ApplySceneModifierCommand, RemoveSceneModifierCommand, SetGraphNodeParamCommand,
};
use manifold_renderer::node_graph::scene_modifier::{
    LOOP_KIND_ID, MIRROR_KIND_ID, build_plan,
};
use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;

fn node(
    id: u32,
    node_id: &str,
    type_id: &str,
    params: BTreeMap<String, SerializedParamValue>,
) -> EffectGraphNode {
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

/// One imported-object group: body carries its `node.scene_object` bind node.
fn object_group(id: u32, handle: &str, bind_id: u32) -> EffectGraphNode {
    use manifold_core::effect_graph_def::{
        GroupDef, GroupInterface, InterfacePortDef, GROUP_TYPE_ID,
    };
    let mut g = node(id, handle, GROUP_TYPE_ID, BTreeMap::new());
    let out_id = bind_id + 1000;
    g.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef {
                name: "object".to_string(),
                port_type: "Object".to_string(),
            }],
            params: Vec::new(),
        },
        nodes: vec![
            node(bind_id, &format!("{handle}_bind"), "node.scene_object", BTreeMap::new()),
            node(out_id, &format!("{handle}_out"), "system.group_output", BTreeMap::new()),
        ],
        wires: vec![wire(bind_id, "object", out_id, "object")],
        tint: None,
    }));
    g
}

/// The import-like scene shape: orbit camera → lens → render_scene, two
/// object groups, scene_bounds with a 5-unit Z extent and the floor at
/// min-Y = -2.0 (the mirror's D8 derivation source).
fn grouped_scene_def() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("MirrorRtScene".to_string()),
            display_name: "Mirror Rt Scene".to_string(),
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
            scene_bounds: Some(([0.0, -2.0, 0.0], [1.0, 1.0, 5.0])),
        }),
        nodes: vec![
            node(0, "camera", "node.orbit_camera", BTreeMap::new()),
            node(1, "lens", "node.camera_lens", BTreeMap::new()),
            node(2, "render", RENDER_SCENE_TYPE_ID, BTreeMap::new()),
            object_group(10, "object_0", 11),
            object_group(20, "object_1", 21),
        ],
        wires: vec![
            wire(0, "out", 1, "camera"),
            wire(1, "out", 2, "camera"),
            wire(10, "object", 2, "object_0"),
            wire(20, "object", 2, "object_1"),
        ],
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

fn apply_kind(project: &mut Project, idx: usize, kind_id: &str) {
    let render_scene_id = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .expect("render_scene")
        .id;
    let plan = build_plan(
        kind_id,
        project.timeline.layers[idx].generator_graph().expect("graph"),
        render_scene_id,
    )
    .unwrap_or_else(|| panic!("{kind_id} plan builder succeeds"));
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let mut cmd = ApplySceneModifierCommand::new(
        manifold_core::GraphTarget::Generator(layer_id),
        Vec::new(),
        plan,
        empty_def(),
    );
    cmd.execute(project);
}

fn remove_kind(project: &mut Project, idx: usize, kind_id: &str) {
    let render_scene_id = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .expect("render_scene")
        .id;
    let plan = build_plan(
        kind_id,
        project.timeline.layers[idx].generator_graph().expect("graph"),
        render_scene_id,
    )
    .unwrap_or_else(|| panic!("{kind_id} remove plan re-derives"));
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let mut cmd = RemoveSceneModifierCommand::new(
        manifold_core::GraphTarget::Generator(layer_id),
        Vec::new(),
        plan,
    );
    cmd.execute(project);
}

/// (node_id, param) targets of every Scene Mirror section binding — the
/// panel-visible surface, in stamp order.
fn scene_mirror_targets(graph: &EffectGraphDef) -> Vec<(String, String)> {
    let meta = graph.preset_metadata.as_ref().expect("preset metadata");
    let section_ids: std::collections::BTreeSet<&str> = meta
        .params
        .iter()
        .filter(|p| p.section.as_deref() == Some("Scene Mirror"))
        .map(|p| p.id.as_str())
        .collect();
    meta.bindings
        .iter()
        .filter(|b| section_ids.contains(b.id.as_str()))
        .filter_map(|b| match &b.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => {
                Some((node_id.as_str().to_string(), param.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The P2 whitelist — the ONLY binding targets the Scene Mirror section
/// may carry.
const MIRROR_WHITELIST: &[(&str, &str)] = &[
    ("mirror_reflect", "enabled"),
    ("mirror_reflect", "axis"),
    ("mirror_reflect", "plane_offset"),
];

fn assert_mirror_whitelist(graph: &EffectGraphDef, context: &str) {
    let mut targets = scene_mirror_targets(graph);
    targets.sort();
    let mut expected: Vec<(String, String)> = MIRROR_WHITELIST
        .iter()
        .map(|(n, p)| (n.to_string(), p.to_string()))
        .collect();
    expected.sort();
    assert_eq!(
        targets, expected,
        "{context}: Scene Mirror section must be exactly the P2 whitelist"
    );

    // Every section binding must resolve to a live node param.
    for (node_id, param) in &targets {
        let found = graph.nodes.iter().any(|n| {
            n.node_id.as_str() == node_id
                && manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(&n.type_id)
                    .iter()
                    .any(|m| m.name == *param)
        });
        assert!(found, "{context}: Scene Mirror row ({node_id}, {param}) has no live node");
    }
}

/// The round-trip gate: apply → save → reload → migrate + reconcile →
/// the rows are exactly the whitelist, the D8-stamped values survive, a
/// performer edit on Plane Offset survives (BUG-pvbu class), and the
/// D6-ordered remove after the reload restores the original graph.
#[test]
fn scene_mirror_roundtrip_whitelist_rows_stable() {
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Mirror RT",
        LayerType::Generator,
        PresetTypeId::from_string("MirrorRtScene".to_string()),
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(grouped_scene_def());
    apply_kind(&mut project, idx, LOOP_KIND_ID);
    apply_kind(&mut project, idx, MIRROR_KIND_ID);

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph after apply")
        .clone();
    assert_mirror_whitelist(&graph, "after apply");

    // The Enabled row is a toggle, the Axis row carries the enum labels,
    // and the Plane Offset default is the D8-stamped floor (min-Y = -2).
    let meta = graph.preset_metadata.as_ref().unwrap();
    let section_spec = |name: &str| {
        meta.params
            .iter()
            .find(|p| p.section.as_deref() == Some("Scene Mirror") && p.name == name)
            .unwrap_or_else(|| panic!("{name} row stamped"))
            .clone()
    };
    let enabled_spec = section_spec("Enabled");
    assert!(enabled_spec.is_toggle, "the Enabled row renders as a toggle (D9)");
    let axis_spec = section_spec("Axis");
    assert_eq!(
        axis_spec.value_labels,
        vec!["+X", "-X", "+Y", "-Y", "+Z", "-Z"],
        "the Axis row carries the reflect atom's enum labels"
    );
    let offset_spec = section_spec("Plane Offset");
    assert_eq!(offset_spec.default_value, -2.0, "Plane Offset defaults to scene_bounds min-Y (D8)");

    // Performer edit: Plane Offset -2.0 → -2.5 through the row-write
    // command (the bound-row write path), as the panel's row would.
    let offset_doc = graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "mirror_reflect")
        .expect("mirror_reflect")
        .id;
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let mut write = SetGraphNodeParamCommand::new(
        target.clone(),
        offset_doc,
        "plane_offset".to_string(),
        SerializedParamValue::Float { value: -2.5 },
        empty_def(),
    );
    write.execute(&mut project);

    // Save → reload → the app's load migration + reconcile.
    let path = std::env::temp_dir().join(format!(
        "manifold_scene_mirror_rt_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v1");
    let mut reloaded = manifold_io::loader::load_project(&path).expect("load v1");

    for layer in &mut reloaded.timeline.layers {
        if let Some(graph) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) {
            manifold_core::scene_object_migration::migrate_scene_object_wires(graph);
            manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(graph);
            manifold_renderer::node_graph::scene_modifier::migrate_pre_switch_scene_loops(graph);
            assert!(
                !manifold_renderer::node_graph::scene_modifier::migrate_loop_exposure_rows(graph),
                "loop rows stamped at apply must all be present at reload — migration is a no-op"
            );
        }
    }
    reloaded.reconcile_param_manifests();

    let ridx = reloaded
        .timeline
        .layers
        .iter()
        .position(|l| l.layer_id == layer_id)
        .expect("layer survived");
    let reloaded_graph = reloaded.timeline.layers[ridx]
        .generator_graph()
        .expect("graph override survived reload");
    assert_mirror_whitelist(reloaded_graph, "after reload");

    // The minted reflect atom still carries the D8 stamp + the performer
    // edit survived the round trip on the node itself.
    let reflect = reloaded_graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "mirror_reflect")
        .expect("mirror_reflect after reload");
    assert_eq!(
        reflect.params.get("plane_offset"),
        Some(&SerializedParamValue::Float { value: -2.5 }),
        "the performer edit survives save/reload (BUG-pvbu class)"
    );
    assert_eq!(
        reflect.params.get("axis"),
        Some(&SerializedParamValue::Enum { value: 2 }),
        "axis stamp survives"
    );

    // The D6-ordered remove after the reload restores the exact original
    // graph (mirror first, then loop).
    let original = grouped_scene_def();
    remove_kind(&mut reloaded, ridx, MIRROR_KIND_ID);
    remove_kind(&mut reloaded, ridx, LOOP_KIND_ID);

    let after = reloaded.timeline.layers[ridx]
        .generator_graph()
        .expect("graph");
    let flat_after = manifold_core::flatten::flatten_groups(after).expect("flatten after");
    let flat_orig = manifold_core::flatten::flatten_groups(&original).expect("flatten original");
    let wire_set = |g: &EffectGraphDef| -> std::collections::BTreeSet<(u32, String, u32, String)> {
        g.wires
            .iter()
            .map(|w| (w.from_node, w.from_port.clone(), w.to_node, w.to_port.clone()))
            .collect()
    };
    assert_eq!(
        flat_after.nodes, flat_orig.nodes,
        "remove after reload restores the original node set"
    );
    assert_eq!(
        wire_set(&flat_after),
        wire_set(&flat_orig),
        "remove after reload restores the golden wiring"
    );

    let _ = std::fs::remove_file(&path);
}
