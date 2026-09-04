//! SCENE_MODIFIER_FRAMEWORK_DESIGN section 4 INV-M gate for the loop kind
//! (P1): INV-M1 (trace all-or-nothing), INV-M2 (apply/remove exact inverse
//! across THREE layers), INV-M3 (whitelist exactness), INV-M7 (enable
//! toggle = one param write), INV-M8 (pre-switch load migration), INV-M9
//! (apply refuses a partial trace), and the layer-duplication round-trip.
//! P2 adds the same gates for the fog kind (section 3.6 — the generality
//! proof) plus its applicability refusals.
//!
//! All tests drive the REAL descriptor plan builder + REAL generic commands
//! (renderer dev-deps editing, the e2e pattern `scene_loop_e2e_import.rs`
//! established).

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
    FOG_KIND_ID, LOOP_KIND_ID, build_plan, descriptor_for, migrate_loop_exposure_rows,
    migrate_pre_switch_scene_loops, trace_modifier,
};
use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;

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

/// One imported-object group: body carries its `node.scene_object` bind node.
fn object_group(id: u32, handle: &str, bind_id: u32) -> EffectGraphNode {
    use manifold_core::effect_graph_def::{GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef};
    let mut g = node(id, handle, GROUP_TYPE_ID, BTreeMap::new());
    let out_id = bind_id + 1000;
    g.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: Vec::new(),
            outputs: vec![InterfacePortDef { name: "object".to_string(), port_type: "Object".to_string() }],
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
/// object groups, scene_bounds with a 5-unit Z extent (cell = 10 after the
/// D4 gap rule). No system.final_output — these tests trace/apply directly
/// rather than through SceneVm liveness.
fn grouped_scene_def() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("InvGateScene".to_string()),
            display_name: "Inv Gate Scene".to_string(),
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
            scene_bounds: Some(([0.0, 0.0, 0.0], [1.0, 1.0, 5.0])),
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

/// Apply the given modifier kind to a fresh project carrying `def`;
/// returns (project, layer index).
fn applied_kind_project(def: EffectGraphDef, kind_id: &str) -> (Project, usize) {
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Inv Gate",
        LayerType::Generator,
        PresetTypeId::from_string("InvGateScene".to_string()),
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(def);
    let render_scene_id = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .expect("render_scene")
        .id;
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let plan = build_plan(kind_id, project.timeline.layers[idx].generator_graph().expect("graph"), render_scene_id)
        .expect("plan builder succeeds");
    let mut cmd = ApplySceneModifierCommand::new(
        manifold_core::GraphTarget::Generator(layer_id),
        Vec::new(),
        plan,
        empty_def(),
    );
    cmd.execute(&mut project);
    (project, idx)
}

fn applied_project(def: EffectGraphDef) -> (Project, usize) {
    applied_kind_project(def, LOOP_KIND_ID)
}

fn loop_descriptor() -> &'static manifold_renderer::node_graph::scene_modifier::SceneModifierDescriptor {
    descriptor_for(LOOP_KIND_ID).expect("scene_loop registered")
}

/// The import-like scene shape for fog tests: orbit camera → lens →
/// render_scene, scene_bounds None. Fog splices no groups and re-points no
/// ports — the plan's only graph touch is the atmosphere wire.
fn fog_scene_def() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("InvGateFogScene".to_string()),
            display_name: "Inv Gate Fog Scene".to_string(),
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
        nodes: vec![
            node(0, "camera", "node.orbit_camera", BTreeMap::new()),
            node(1, "lens", "node.camera_lens", BTreeMap::new()),
            node(2, "render", RENDER_SCENE_TYPE_ID, BTreeMap::new()),
        ],
        wires: vec![
            wire(0, "out", 1, "camera"),
            wire(1, "out", 2, "camera"),
        ],
    }
}

fn fog_descriptor() -> &'static manifold_renderer::node_graph::scene_modifier::SceneModifierDescriptor {
    descriptor_for(FOG_KIND_ID).expect("scene_fog registered")
}

/// INV-M1: the trace is all-or-nothing per kind — full graph → applied;
/// deleting ANY required node → not applied. The optional camera switch is
/// not required (a hand-deleted switch leaves the kind applied but
/// un-toggleable).
#[test]
fn inv_m1_trace_is_all_or_nothing() {
    let (project, idx) = applied_project(grouped_scene_def());
    let graph = project.timeline.layers[idx].generator_graph().expect("graph");
    let d = loop_descriptor();
    let result = trace_modifier(d, &graph.nodes);
    assert!(result.applied(d), "fresh apply must trace as applied");
    assert!(result.doc_ids.contains_key("loop_cam_switch"), "switch traced as optional node");

    // Deleting any REQUIRED node drops the trace to not-applied.
    for (node_id, type_id) in [
        ("loop_phase", "node.beat_ramp"),
        ("scene_array", "node.scene_array"),
        ("loop_camera", "node.loop_camera"),
    ] {
        let mut broken = graph.clone();
        broken.nodes.retain(|n| !(n.node_id.as_str() == node_id && n.type_id == type_id));
        let r = trace_modifier(d, &broken.nodes);
        assert!(!r.applied(d), "INV-M1: deleting {node_id} must read as not applied");
        assert!(r.partial(d), "INV-M1: deleting {node_id} is the partial state");
    }

    // Deleting the OPTIONAL switch keeps the kind applied (D3/D8).
    let mut no_switch = graph.clone();
    no_switch.nodes.retain(|n| n.node_id.as_str() != "loop_cam_switch");
    let r = trace_modifier(d, &no_switch.nodes);
    assert!(r.applied(d), "INV-M1: the optional switch is not required for applied");
    assert!(!r.partial(d), "INV-M1: a missing optional node is not partial");
}

/// INV-M3: the stamped rows are EXACTLY the kind whitelist — no atom
/// internals leak (the pre-P4 duplicate-Axis class).
#[test]
fn inv_m3_stamped_rows_match_whitelist_exactly() {
    let (project, idx) = applied_project(grouped_scene_def());
    let graph = project.timeline.layers[idx].generator_graph().expect("graph");
    let meta = graph.preset_metadata.as_ref().expect("metadata stamped");
    let section_ids: std::collections::BTreeSet<&str> = meta
        .params
        .iter()
        .filter(|p| p.section.as_deref() == Some("Scene Loop"))
        .map(|p| p.id.as_str())
        .collect();
    let targets: std::collections::BTreeSet<(String, String)> = meta
        .bindings
        .iter()
        .filter(|b| section_ids.contains(b.id.as_str()))
        .filter_map(|b| match &b.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => {
                Some((node_id.as_str().to_string(), param.clone()))
            }
            _ => None,
        })
        .collect();
    let expected: std::collections::BTreeSet<(String, String)> = [
        ("loop_phase", "bars"),
        ("scene_array", "count"),
        ("loop_camera", "height"),
        ("loop_camera", "lateral"),
        ("loop_camera", "flow"),
        ("loop_camera", "stride"),
        ("loop_camera", "sway_amp"),
        ("loop_camera", "sway_cycles"),
        ("loop_camera", "look_sweep_amp"),
        ("loop_camera", "zoom_pulse_amp"),
        ("loop_camera", "cell_size"),
        ("scene_array", "jitter_amount"),
    ]
    .iter()
    .map(|(n, p)| (n.to_string(), p.to_string()))
    .collect();
    assert_eq!(
        targets, expected,
        "INV-M3: Scene Loop section rows must be exactly the whitelist"
    );
}

/// P4 load migration (INV-M3 extension): a loop applied BEFORE the control
/// enrichment carries only the four D6 rows. Stripping the new rows to
/// simulate that project, one load migration must stamp exactly the new P4
/// rows — old rows untouched (ids preserved), Spacing's range curated to
/// auto×0.25..4.0, and the migration idempotent (a second run changes
/// nothing).
#[test]
fn p4_migration_stamps_new_rows_on_pre_enrichment_loops() {
    let (project, idx) = applied_project(grouped_scene_def());
    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .clone();
    let mut def = graph;

    // Simulate the pre-P4 project: drop every Scene Loop row except the
    // four D6 rows, exactly what an old save carries.
    let old_targets: std::collections::BTreeSet<(&str, &str)> = [
        ("loop_phase", "bars"),
        ("scene_array", "count"),
        ("loop_camera", "height"),
        ("loop_camera", "lateral"),
    ]
    .into_iter()
    .collect();
    let meta = def.preset_metadata.as_mut().unwrap();
    let keep_ids: std::collections::BTreeSet<String> = meta
        .bindings
        .iter()
        .filter(|b| {
            matches!(
                &b.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                    if old_targets.contains(&(node_id.as_str(), param.as_str()))
            )
        })
        .map(|b| b.id.clone())
        .collect();
    meta.params.retain(|p| keep_ids.contains(p.id.as_str()));
    meta.bindings.retain(|b| keep_ids.contains(b.id.as_str()));
    let old_param_count = meta.params.len();
    assert_eq!(old_param_count, 4, "fixture starts from the four D6 rows");

    // One migration: the eight new rows land.
    assert!(
        migrate_loop_exposure_rows(&mut def),
        "the first migration must stamp the new rows"
    );
    let meta = def.preset_metadata.as_ref().unwrap();
    let section_rows = meta
        .params
        .iter()
        .filter(|p| p.section.as_deref() == Some("Scene Loop"))
        .count();
    assert_eq!(section_rows, 12, "4 old + 8 new rows after migration");

    // The old four ids are untouched (no re-stamp churn).
    for id in &keep_ids {
        assert!(
            meta.bindings.iter().any(|b| &b.id == id),
            "old row {id} must survive the migration"
        );
    }

    // Spacing row: curated range against the node's cell (auto = 10 for
    // the 5-unit Z extent ×2 gap rule), default = the auto cell.
    let spacing = meta
        .params
        .iter()
        .find(|p| p.name == "Spacing")
        .expect("Spacing row stamped by the migration");
    assert_eq!((spacing.min, spacing.max), (2.5, 40.0), "auto×0.25..4.0");

    // Idempotent: a second load changes nothing.
    assert!(
        !migrate_loop_exposure_rows(&mut def),
        "the migration must be a no-op once every row is stamped"
    );

    // No loop applied → no stamping at all.
    assert!(!migrate_loop_exposure_rows(&mut empty_def()));
}

/// INV-M2: apply → remove are exact inverses across THREE layers: the
/// flattened graph equals the original, no manifest params carry the kind's
/// section, and no drivers/envelopes/Ableton mappings target the stripped
/// binding ids (the BUG-6vv7 (scene-loop-remove-orphan-presetinstance-params) fix).
#[test]
fn inv_m2_apply_remove_exact_inverse_three_layers() {
    let original = grouped_scene_def();
    let (mut project, idx) = applied_project(original.clone());
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());

    // Attach modulation to two stamped rows (the performer's surface): a
    // driver on Bars, an envelope on Copies, an Ableton mapping on Height.
    let stamped_ids: Vec<String> = {
        let graph = project.timeline.layers[idx].generator_graph().expect("graph");
        let meta = graph.preset_metadata.as_ref().unwrap();
        meta.bindings
            .iter()
            .filter(|b| {
                matches!(
                    &b.target,
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. }
                        if matches!(node_id.as_str(), "loop_phase" | "scene_array" | "loop_camera")
                )
            })
            .map(|b| b.id.clone())
            .collect()
    };
    assert_eq!(stamped_ids.len(), 12, "twelve whitelist rows stamped");
    project
        .with_preset_graph_mut(&target, |inst| {
            inst.drivers = Some(vec![manifold_core::effects::ParameterDriver {
                param_id: std::borrow::Cow::Owned(stamped_ids[0].clone()),
                beat_division: manifold_core::types::BeatDivision::Quarter,
                waveform: manifold_core::types::DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.0,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            inst.envelopes = Some(vec![manifold_core::effects::ParamEnvelope::new(
                stamped_ids[1].clone(),
            )]);
            inst.ableton_mappings = Some(vec![manifold_core::ableton_mapping::AbletonParamMapping {
                param_id: std::borrow::Cow::Owned(stamped_ids[2].clone()),
                address: manifold_core::ableton_mapping::AbletonMacroAddress {
                    track_id: 0,
                    device_id: 0,
                    param_id: 0,
                    device_identity: manifold_core::ableton_mapping::AbletonDeviceIdentity {
                        device_class_name: "TestDevice".to_string(),
                    },
                    track_name: "T".to_string(),
                    device_name: "D".to_string(),
                    macro_name: "M".to_string(),
                },
                range_min: 0.0,
                range_max: 1.0,
                inverted: false,
                legacy_param_index: None,
                last_value: 0.0,
                status: Default::default(),
            }]);
        })
        .expect("instance reachable");

    // Remove: re-derive the plan from the current graph (the dispatch path).
    let render_scene_id = original
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .map(|n| n.id)
        .unwrap();
    let remove_plan = build_plan(
        LOOP_KIND_ID,
        project.timeline.layers[idx].generator_graph().expect("graph"),
        render_scene_id,
    )
    .expect("remove plan re-derives");
    let mut remove = RemoveSceneModifierCommand::new(target.clone(), Vec::new(), remove_plan);
    remove.execute(&mut project);

    // Layer 1 + 2: the flattened graph equals the original, and no
    // preset_metadata row carries the kind's section.
    let after = project.timeline.layers[idx].generator_graph().expect("graph");
    let flat_after = manifold_core::flatten::flatten_groups(after).expect("flatten after");
    let flat_orig = manifold_core::flatten::flatten_groups(&original).expect("flatten original");
    // Wire ORDER is not topology: the remove appends the restored camera
    // re-point at the tail, so compare sets.
    let wire_set = |g: &EffectGraphDef| -> std::collections::BTreeSet<(u32, String, u32, String)> {
        g.wires
            .iter()
            .map(|w| (w.from_node, w.from_port.clone(), w.to_node, w.to_port.clone()))
            .collect()
    };
    assert_eq!(
        flat_after.nodes, flat_orig.nodes,
        "INV-M2: apply → remove must restore the original node set"
    );
    assert_eq!(
        wire_set(&flat_after),
        wire_set(&flat_orig),
        "INV-M2: apply → remove must restore the original wires (re-point restored)"
    );
    let meta = after.preset_metadata.as_ref().expect("metadata survives");
    assert!(
        meta.params.iter().all(|p| p.section.as_deref() != Some("Scene Loop")),
        "INV-M2: no preset_metadata params carry the kind's section after remove"
    );
    assert!(
        meta.bindings.iter().all(|b| match &b.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                !matches!(node_id.as_str(), "loop_phase" | "scene_array" | "loop_camera" | "loop_cam_switch")
            }
            _ => true,
        }),
        "INV-M2: no bindings target the minted node ids after remove"
    );

    // Layer 3: the instance carries no params or modulation targeting the
    // stripped ids.
    let layer = project.timeline.layers[idx].clone();
    let gp = layer.gen_params().expect("gen params");
    for id in &stamped_ids {
        assert!(
            gp.params.get(id).is_none(),
            "INV-M2: orphan manifest param {id:?} must be pruned (BUG-6vv7 class)"
        );
    }
    assert!(
        gp.drivers.as_ref().map(|ds| ds.iter().all(|d| !stamped_ids.iter().any(|id| d.param_id == id.as_str()))).unwrap_or(true),
        "INV-M2: drivers targeting stripped ids must be pruned"
    );
    assert!(
        gp.envelopes.as_ref().map(|es| es.iter().all(|e| !stamped_ids.iter().any(|id| e.param_id == id.as_str()))).unwrap_or(true),
        "INV-M2: envelopes targeting stripped ids must be pruned"
    );
    assert!(
        gp.ableton_mappings.as_ref().map(|ms| ms.iter().all(|m| !stamped_ids.iter().any(|id| m.param_id == id.as_str()))).unwrap_or(true),
        "INV-M2: Ableton mappings targeting stripped ids must be pruned"
    );

    // Undo restores all three layers.
    remove.undo(&mut project);
    let restored = project.timeline.layers[idx].generator_graph().expect("graph");
    assert!(
        restored.nodes.iter().any(|n| n.node_id.as_str() == "loop_cam_switch"),
        "undo restores the minted nodes"
    );
    let layer = project.timeline.layers[idx].clone();
    let gp = layer.gen_params().expect("gen params");
    assert!(
        stamped_ids.iter().all(|id| gp.params.get(id).is_some()),
        "undo restores the pruned manifest params"
    );
    assert!(gp.drivers.is_some(), "undo restores the pruned drivers");
}

/// INV-M7: the enable toggle is exactly one param write on the switch's
/// `select` — the graph (nodes + wires) is byte-identical before/after, and
/// undo restores the value.
#[test]
fn inv_m7_enable_toggle_is_one_param_write() {
    let (mut project, idx) = applied_project(grouped_scene_def());
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let graph = project.timeline.layers[idx].generator_graph().expect("graph").clone();
    let switch_id = graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_cam_switch")
        .map(|n| n.id)
        .expect("switch minted");

    // Toggle OFF: select B → A (one param write through the same command a
    // manifest row uses).
    let mut toggle = SetGraphNodeParamCommand::new(
        target,
        switch_id,
        "select".to_string(),
        SerializedParamValue::Enum { value: 0 },
        empty_def(),
    );
    toggle.execute(&mut project);

    let after = project.timeline.layers[idx].generator_graph().expect("graph");
    // INV-M7: topology is untouched — same node ids/types, same wires. The
    // toggle's ONLY effect is the select value write (asserted below), so
    // the node structs are compared with that value normalized out.
    // Node (id, node_id, type_id) and wire (from, from_port, to, to_port)
    // projections compared before/after the toggle.
    type Topo = (Vec<(u32, String, String)>, Vec<(u32, String, u32, String)>);
    let topo = |g: &EffectGraphDef| -> Topo {
            (
                g.nodes
                    .iter()
                    .map(|n| (n.id, n.node_id.as_str().to_string(), n.type_id.clone()))
                    .collect(),
                g.wires
                    .iter()
                    .map(|w| {
                        (
                            w.from_node,
                            w.from_port.clone(),
                            w.to_node,
                            w.to_port.clone(),
                        )
                    })
                    .collect(),
            )
        };
    assert_eq!(topo(after), topo(&graph), "INV-M7: toggle changes no topology");
    // Exactly one param write landed, on the switch's select.
    let changed: Vec<_> = after
        .nodes
        .iter()
        .zip(graph.nodes.iter())
        .filter(|(a, b)| a.params != b.params)
        .map(|(a, _)| a.node_id.as_str())
        .collect();
    assert_eq!(changed, vec!["loop_cam_switch"], "INV-M7: one param write, on the switch");
    let select = after
        .nodes
        .iter()
        .find(|n| n.id == switch_id)
        .and_then(|n| n.params.get("select"))
        .cloned();
    assert_eq!(select, Some(SerializedParamValue::Enum { value: 0 }), "toggle wrote select = A");

    toggle.undo(&mut project);
    let restored = project.timeline.layers[idx].generator_graph().expect("graph");
    let select = restored
        .nodes
        .iter()
        .find(|n| n.id == switch_id)
        .and_then(|n| n.params.get("select"))
        .cloned();
    assert_eq!(select, Some(SerializedParamValue::Enum { value: 1 }), "undo restores select = B");
}

/// INV-M8: an old (pre-switch) looped project loads, traces applied, and
/// migrates ONCE — the minted switch re-points the camera path, select = B
/// (enabled), and a second migration run is a no-op.
#[test]
fn inv_m8_pre_switch_graph_migrates_once_at_load() {
    // The pre-switch applied shape: the three atoms, loop_camera wired
    // DIRECTLY into lens.camera. The orbit camera node is still in the
    // graph — its wire was dropped at apply, not the node — so drop it here
    // to reproduce that state faithfully.
    let mut def = grouped_scene_def();
    def.wires.retain(|w| !(w.from_node == 0 && w.to_node == 1 && w.to_port == "camera"));
    let mut phase_params = BTreeMap::new();
    phase_params.insert("bars".to_string(), SerializedParamValue::Float { value: 8.0 });
    phase_params.insert("rate".to_string(), SerializedParamValue::Float { value: 0.0 });
    phase_params.insert("attack".to_string(), SerializedParamValue::Float { value: 1.0 });
    let mut array_params = BTreeMap::new();
    array_params.insert("count".to_string(), SerializedParamValue::Float { value: 3.0 });
    array_params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    array_params.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
    let mut camera_params = BTreeMap::new();
    camera_params.insert("cell_size".to_string(), SerializedParamValue::Float { value: 10.0 });
    camera_params.insert("axis".to_string(), SerializedParamValue::Enum { value: 4 });
    def.nodes.push(node(30, "loop_phase", "node.beat_ramp", phase_params));
    def.nodes.push(node(31, "scene_array", "node.scene_array", array_params));
    def.nodes.push(node(32, "loop_camera", "node.loop_camera", camera_params));
    def.wires.push(wire(30, "out", 32, "phase"));
    def.wires.push(wire(32, "out", 1, "camera")); // direct pre-switch shape

    let d = loop_descriptor();
    let pre = trace_modifier(d, &def.nodes);
    assert!(pre.applied(d), "pre-switch graph traces applied");
    assert!(!pre.doc_ids.contains_key("loop_cam_switch"), "pre-switch graph has no switch");

    assert!(migrate_pre_switch_scene_loops(&mut def), "first migration mints the switch");
    let post = trace_modifier(d, &def.nodes);
    assert!(post.applied(d), "migrated graph still traces applied");
    let switch_id = post.doc_ids["loop_cam_switch"];
    assert_eq!(
        def.nodes
            .iter()
            .find(|n| n.id == switch_id)
            .and_then(|n| n.params.get("select")),
        Some(&SerializedParamValue::Enum { value: 1 }),
        "migration preserves the enabled state (select = B)"
    );
    assert!(
        def.wires.iter().any(|w| w.from_node == 0 && w.to_node == switch_id && w.to_port == "a"),
        "the original orbit camera feeds the switch's a input"
    );
    assert!(
        def.wires.iter().any(|w| w.from_node == 32 && w.to_node == switch_id && w.to_port == "b"),
        "loop_camera feeds the switch's b input"
    );
    assert!(
        def.wires.iter().any(|w| w.from_node == switch_id && w.to_node == 1 && w.to_port == "camera"),
        "the switch re-points into lens.camera"
    );
    assert!(
        !def.wires.iter().any(|w| w.from_node == 32 && w.to_node == 1),
        "the direct loop_camera → lens wire is replaced"
    );

    assert!(
        !migrate_pre_switch_scene_loops(&mut def),
        "INV-M8: the migration is idempotent (second run is a no-op)"
    );
}

/// INV-M9: apply refuses a PARTIAL trace — a hand-deleted required node
/// makes the apply a logged no-op, and no nodeIds are duplicated.
#[test]
fn inv_m9_apply_refuses_partial_trace() {
    let (mut project, idx) = applied_project(grouped_scene_def());
    let render_scene_id = 2;

    // Hand-edit debris: delete the scene_array node (and its wires).
    {
        let layer = &mut project.timeline.layers[idx];
        let graph = layer.gen_params_mut().unwrap().graph.as_mut().unwrap();
        graph.nodes.retain(|n| n.node_id.as_str() != "scene_array");
        graph.wires.retain(|w| w.to_port != "instances");
    }

    let before = project.timeline.layers[idx].generator_graph().expect("graph").clone();
    let plan = build_plan(
        LOOP_KIND_ID,
        project.timeline.layers[idx].generator_graph().expect("graph"),
        render_scene_id,
    )
    .expect("plan still builds on the broken graph");
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let mut cmd = ApplySceneModifierCommand::new(
        manifold_core::GraphTarget::Generator(layer_id),
        Vec::new(),
        plan,
        empty_def(),
    );
    cmd.execute(&mut project);

    let after = project.timeline.layers[idx].generator_graph().expect("graph");
    assert_eq!(
        after.nodes.len(),
        before.nodes.len(),
        "INV-M9: a partial-trace apply must add no nodes"
    );
    assert_eq!(
        after.wires.len(),
        before.wires.len(),
        "INV-M9: a partial-trace apply must add no wires"
    );
    let mut ids: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for n in &after.nodes {
        assert!(ids.insert(n.node_id.as_str()), "INV-M9: duplicated nodeId {}", n.node_id);
    }
}

/// Layer-duplication round-trip: duplicating a looped layer gives both
/// layers independent traces — removing the modifier from one leaves the
/// other applied.
#[test]
fn layer_duplication_roundtrip_traces_independent() {
    let (mut project, idx_a) = applied_project(grouped_scene_def());
    let d = loop_descriptor();

    // Duplicate: a second layer carrying a clone of the applied graph.
    let applied_graph = project.timeline.layers[idx_a].generator_graph().expect("graph").clone();
    let idx_b = project.timeline.add_layer(
        "Inv Gate Dup",
        LayerType::Generator,
        PresetTypeId::from_string("InvGateScene".to_string()),
    );
    project.timeline.layers[idx_b].gen_params_or_init().graph = Some(applied_graph.clone());

    for idx in [idx_a, idx_b] {
        let graph = project.timeline.layers[idx].generator_graph().expect("graph");
        let r = trace_modifier(d, &graph.nodes);
        assert!(r.applied(d), "layer {idx} traces applied after duplication");
    }

    // Remove from B only.
    let layer_b = project.timeline.layers[idx_b].layer_id.clone();
    let render_scene_id = applied_graph
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .map(|n| n.id)
        .unwrap();
    let remove_plan = build_plan(
        LOOP_KIND_ID,
        project.timeline.layers[idx_b].generator_graph().expect("graph"),
        render_scene_id,
    )
    .expect("remove plan on the duplicate");
    let mut remove = RemoveSceneModifierCommand::new(
        manifold_core::GraphTarget::Generator(layer_b),
        Vec::new(),
        remove_plan,
    );
    remove.execute(&mut project);

    let graph_a = project.timeline.layers[idx_a].generator_graph().expect("graph");
    assert!(
        trace_modifier(d, &graph_a.nodes).applied(d),
        "layer A stays applied after removing from the duplicate"
    );
    let graph_b = project.timeline.layers[idx_b].generator_graph().expect("graph");
    assert!(
        !trace_modifier(d, &graph_b.nodes).applied(d),
        "layer B reads not-applied after its remove"
    );
    assert!(
        graph_b.nodes.iter().all(|n| !matches!(n.node_id.as_str(), "loop_phase" | "scene_array" | "loop_camera" | "loop_cam_switch")),
        "layer B's minted nodes are gone"
    );
}

// ---------------------------------------------------------------------------
// P2 — the fog kind (section 3.6, the generality proof)
// ---------------------------------------------------------------------------

/// INV-M1 (fog): the trace is all-or-nothing — deleting ANY of the four
/// minted nodes reads not-applied AND partial.
#[test]
fn inv_m1_fog_trace_is_all_or_nothing() {
    let (project, idx) = applied_kind_project(fog_scene_def(), FOG_KIND_ID);
    let graph = project.timeline.layers[idx].generator_graph().expect("graph");
    let d = fog_descriptor();
    let result = trace_modifier(d, &graph.nodes);
    assert!(result.applied(d), "fresh apply must trace as applied");
    for node_id in ["fog_atm", "fog_enabled", "fog_amount", "fog_mul"] {
        assert!(
            result.doc_ids.contains_key(node_id),
            "{node_id} must resolve in the trace"
        );
    }

    for (node_id, type_id) in [
        ("fog_atm", "node.atmosphere"),
        ("fog_enabled", "node.value"),
        ("fog_amount", "node.value"),
        ("fog_mul", "node.math"),
    ] {
        let mut broken = graph.clone();
        broken.nodes.retain(|n| !(n.node_id.as_str() == node_id && n.type_id == type_id));
        let r = trace_modifier(d, &broken.nodes);
        assert!(!r.applied(d), "INV-M1: deleting {node_id} must read as not applied");
        assert!(r.partial(d), "INV-M1: deleting {node_id} is the partial state");
    }
}

/// INV-M3 (fog): the stamped rows are EXACTLY the kind whitelist — the two
/// value atoms' `value` params, nothing from fog_atm's internals.
#[test]
fn inv_m3_fog_stamped_rows_match_whitelist_exactly() {
    let (project, idx) = applied_kind_project(fog_scene_def(), FOG_KIND_ID);
    let graph = project.timeline.layers[idx].generator_graph().expect("graph");
    let meta = graph.preset_metadata.as_ref().expect("metadata stamped");
    let section_ids: std::collections::BTreeSet<&str> = meta
        .params
        .iter()
        .filter(|p| p.section.as_deref() == Some("Scene Fog"))
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(section_ids.len(), 2, "exactly two Scene Fog rows stamped");
    let targets: std::collections::BTreeSet<(String, String)> = meta
        .bindings
        .iter()
        .filter(|b| section_ids.contains(b.id.as_str()))
        .filter_map(|b| match &b.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => {
                Some((node_id.as_str().to_string(), param.clone()))
            }
            _ => None,
        })
        .collect();
    let expected: std::collections::BTreeSet<(String, String)> = [
        ("fog_enabled", "value"),
        ("fog_amount", "value"),
    ]
    .iter()
    .map(|(n, p)| (n.to_string(), p.to_string()))
    .collect();
    assert_eq!(
        targets, expected,
        "INV-M3: Scene Fog section rows must be exactly the whitelist"
    );

    // The Enabled row carries the toggle curation (D5); Density is a slider.
    let enabled_spec = meta
        .params
        .iter()
        .find(|p| section_ids.contains(p.id.as_str()) && p.name == "Enabled")
        .expect("Enabled row stamped");
    assert!(enabled_spec.is_toggle, "the Enabled row must render as a toggle");
    let density_spec = meta
        .params
        .iter()
        .find(|p| section_ids.contains(p.id.as_str()) && p.name == "Density")
        .expect("Density row stamped");
    assert!(!density_spec.is_toggle, "the Density row is a slider");
}

/// INV-M2 (fog): apply → remove are exact inverses across THREE layers —
/// the flattened graph equals the original, no manifest params carry the
/// "Scene Fog" section, and drivers/envelopes targeting the stripped
/// binding ids are pruned (undo restores all of it).
#[test]
fn inv_m2_fog_apply_remove_exact_inverse_three_layers() {
    let original = fog_scene_def();
    let (mut project, idx) = applied_kind_project(original.clone(), FOG_KIND_ID);
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());

    // Attach modulation to both stamped rows: a driver on Enabled, an
    // envelope on Density.
    let stamped_ids: Vec<String> = {
        let graph = project.timeline.layers[idx].generator_graph().expect("graph");
        let meta = graph.preset_metadata.as_ref().unwrap();
        meta.bindings
            .iter()
            .filter(|b| {
                matches!(
                    &b.target,
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. }
                        if matches!(node_id.as_str(), "fog_enabled" | "fog_amount")
                )
            })
            .map(|b| b.id.clone())
            .collect()
    };
    assert_eq!(stamped_ids.len(), 2, "two whitelist rows stamped");
    project
        .with_preset_graph_mut(&target, |inst| {
            inst.drivers = Some(vec![manifold_core::effects::ParameterDriver {
                param_id: std::borrow::Cow::Owned(stamped_ids[0].clone()),
                beat_division: manifold_core::types::BeatDivision::Quarter,
                waveform: manifold_core::types::DriverWaveform::Sine,
                enabled: true,
                phase: 0.0,
                base_value: 0.0,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                free_period_beats: None,
                legacy_param_index: None,
                is_paused_by_user: false,
            }]);
            inst.envelopes = Some(vec![manifold_core::effects::ParamEnvelope::new(
                stamped_ids[1].clone(),
            )]);
        })
        .expect("instance reachable");

    // Remove: re-derive the plan from the current graph (the dispatch path).
    let render_scene_id = original
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .map(|n| n.id)
        .unwrap();
    let remove_plan = build_plan(
        FOG_KIND_ID,
        project.timeline.layers[idx].generator_graph().expect("graph"),
        render_scene_id,
    )
    .expect("remove plan re-derives");
    let mut remove = RemoveSceneModifierCommand::new(target.clone(), Vec::new(), remove_plan);
    remove.execute(&mut project);

    // Layer 1 + 2: the flattened graph equals the original, and no
    // preset_metadata row carries the kind's section.
    let after = project.timeline.layers[idx].generator_graph().expect("graph");
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
        "INV-M2: apply → remove must restore the original node set"
    );
    assert_eq!(
        wire_set(&flat_after),
        wire_set(&flat_orig),
        "INV-M2: apply → remove must restore the original wires"
    );
    let meta = after.preset_metadata.as_ref().expect("metadata survives");
    assert!(
        meta.params.iter().all(|p| p.section.as_deref() != Some("Scene Fog")),
        "INV-M2: no preset_metadata params carry the kind's section after remove"
    );
    assert!(
        meta.bindings.iter().all(|b| match &b.target {
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, .. } => {
                !matches!(node_id.as_str(), "fog_atm" | "fog_enabled" | "fog_amount" | "fog_mul")
            }
            _ => true,
        }),
        "INV-M2: no bindings target the minted node ids after remove"
    );

    // Layer 3: the instance carries no params or modulation targeting the
    // stripped ids.
    let layer = project.timeline.layers[idx].clone();
    let gp = layer.gen_params().expect("gen params");
    for id in &stamped_ids {
        assert!(
            gp.params.get(id).is_none(),
            "INV-M2: orphan manifest param {id:?} must be pruned"
        );
    }
    assert!(
        gp.drivers.as_ref().map(|ds| ds.iter().all(|d| !stamped_ids.iter().any(|id| d.param_id == id.as_str()))).unwrap_or(true),
        "INV-M2: drivers targeting stripped ids must be pruned"
    );
    assert!(
        gp.envelopes.as_ref().map(|es| es.iter().all(|e| !stamped_ids.iter().any(|id| e.param_id == id.as_str()))).unwrap_or(true),
        "INV-M2: envelopes targeting stripped ids must be pruned"
    );

    // Undo restores all three layers.
    remove.undo(&mut project);
    let restored = project.timeline.layers[idx].generator_graph().expect("graph");
    assert!(
        restored.nodes.iter().any(|n| n.node_id.as_str() == "fog_atm"),
        "undo restores the minted nodes"
    );
    let layer = project.timeline.layers[idx].clone();
    let gp = layer.gen_params().expect("gen params");
    assert!(
        stamped_ids.iter().all(|id| gp.params.get(id).is_some()),
        "undo restores the pruned manifest params"
    );
    assert!(gp.drivers.is_some(), "undo restores the pruned drivers");
    assert!(gp.envelopes.is_some(), "undo restores the pruned envelopes");
}

/// INV-M7 (fog): the enable toggle is exactly one param write on the
/// enabled value atom's `value` — topology byte-identical before/after,
/// undo restores the value. The def-level gate wiring is asserted here;
/// the arithmetic (enabled = 0 → fog_density 0) is the value-level proof
/// `scene_modifier_fog::tests::gate_bypass_multiplies_enabled_by_amount`.
#[test]
fn inv_m7_fog_enable_toggle_is_one_param_write() {
    let (mut project, idx) = applied_kind_project(fog_scene_def(), FOG_KIND_ID);
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let graph = project.timeline.layers[idx].generator_graph().expect("graph").clone();

    let doc = |g: &EffectGraphDef, node_id: &str| -> u32 {
        g.nodes
            .iter()
            .find(|n| n.node_id.as_str() == node_id)
            .map(|n| n.id)
            .unwrap_or_else(|| panic!("{node_id} minted"))
    };
    let (atm, enabled, amount, mul) =
        (doc(&graph, "fog_atm"), doc(&graph, "fog_enabled"), doc(&graph, "fog_amount"), doc(&graph, "fog_mul"));

    // The D5 gate wiring: enabled × amount → fog_atm.fog_density, fog_atm
    // → render_scene.atmosphere, and the math op is Multiply.
    assert_eq!(
        graph.nodes.iter().find(|n| n.id == mul).and_then(|n| n.params.get("op")),
        Some(&SerializedParamValue::Enum { value: 2 }),
        "the gate math is Multiply"
    );
    for (f, fport, t, tport) in [
        (enabled, "out", mul, "a"),
        (amount, "out", mul, "b"),
        (mul, "out", atm, "fog_density"),
        (atm, "atmosphere", 2, "atmosphere"),
    ] {
        assert!(
            graph
                .wires
                .iter()
                .any(|w| w.from_node == f && w.from_port == fport && w.to_node == t && w.to_port == tport),
            "gate wire {fport} → {tport} must exist"
        );
    }
    assert_eq!(
        graph
            .wires
            .iter()
            .filter(|w| w.to_node == atm && w.to_port == "fog_density")
            .count(),
        1,
        "fog_atm.fog_density has exactly one producer — the gate is total, no param fallback"
    );

    // Toggle OFF: one param write through the same command a manifest row
    // uses (apply_scene_param_write's unbound def-level fallback shape).
    let mut toggle = SetGraphNodeParamCommand::new(
        target,
        enabled,
        "value".to_string(),
        SerializedParamValue::Float { value: 0.0 },
        empty_def(),
    );
    toggle.execute(&mut project);

    let after = project.timeline.layers[idx].generator_graph().expect("graph");
    type Topo = (Vec<(u32, String, String)>, Vec<(u32, String, u32, String)>);
    let topo = |g: &EffectGraphDef| -> Topo {
        (
            g.nodes
                .iter()
                .map(|n| (n.id, n.node_id.as_str().to_string(), n.type_id.clone()))
                .collect(),
            g.wires
                .iter()
                .map(|w| (w.from_node, w.from_port.clone(), w.to_node, w.to_port.clone()))
                .collect(),
        )
    };
    assert_eq!(topo(after), topo(&graph), "INV-M7: toggle changes no topology");
    let changed: Vec<_> = after
        .nodes
        .iter()
        .zip(graph.nodes.iter())
        .filter(|(a, b)| a.params != b.params)
        .map(|(a, _)| a.node_id.as_str())
        .collect();
    assert_eq!(changed, vec!["fog_enabled"], "INV-M7: one param write, on the enabled atom");
    let value = after
        .nodes
        .iter()
        .find(|n| n.id == enabled)
        .and_then(|n| n.params.get("value"))
        .cloned();
    assert_eq!(value, Some(SerializedParamValue::Float { value: 0.0 }), "toggle wrote value = 0");

    toggle.undo(&mut project);
    let restored = project.timeline.layers[idx].generator_graph().expect("graph");
    let value = restored
        .nodes
        .iter()
        .find(|n| n.id == enabled)
        .and_then(|n| n.params.get("value"))
        .cloned();
    assert_eq!(value, Some(SerializedParamValue::Float { value: 1.0 }), "undo restores value = 1");
}

/// Applicability (section 3.6 + the K3 amendments): a fresh single scene is
/// applicable; a foreign atmosphere producer, a second render_scene, or an
/// applied Atmosphere-group kind each grey the picker. The plan builder
/// stays permissive in every case — the remove arm re-derives through it.
#[test]
fn fog_applicability_refusals() {
    let d = fog_descriptor();

    // Fresh scene → applicable.
    let def = fog_scene_def();
    assert!((d.applicable)(&def, 2), "a fresh single scene offers fog");

    // Foreign atmosphere producer → greyed; the plan builder still succeeds
    // (the P1 remove-re-derivation contract).
    let mut foreign = fog_scene_def();
    foreign.nodes.push(node(3, "haze", "node.atmosphere", BTreeMap::new()));
    foreign.wires.push(wire(3, "atmosphere", 2, "atmosphere"));
    assert!(!(d.applicable)(&foreign, 2), "an existing atmosphere producer greys the picker");
    assert!(
        (d.plan_builder)(&foreign, 2).is_some(),
        "the plan builder stays permissive — remove re-derives through it"
    );

    // Two render_scenes (INV-M6) → greyed.
    let mut multi = fog_scene_def();
    multi.nodes.push(node(3, "render2", RENDER_SCENE_TYPE_ID, BTreeMap::new()));
    assert!(!(d.applicable)(&multi, 2), "multi-scene greys the picker");

    // Same-group applied (fog itself occupies the Atmosphere slot) →
    // greyed, and a dispatched re-apply refuses at the command layer
    // (the fully-applied trace check, the INV-M9-adjacent guard).
    let (mut project, idx) = applied_kind_project(fog_scene_def(), FOG_KIND_ID);
    let applied_graph = project.timeline.layers[idx].generator_graph().expect("graph").clone();
    assert!(!(d.applicable)(&applied_graph, 2), "an applied Atmosphere kind occupies the slot");

    let plan = build_plan(FOG_KIND_ID, &applied_graph, 2).expect("plan builds on the applied graph");
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let mut cmd = ApplySceneModifierCommand::new(
        manifold_core::GraphTarget::Generator(layer_id),
        Vec::new(),
        plan,
        empty_def(),
    );
    cmd.execute(&mut project);
    let after = project.timeline.layers[idx].generator_graph().expect("graph");
    assert_eq!(
        after.nodes.len(),
        applied_graph.nodes.len(),
        "a fully-applied re-apply must add no nodes"
    );
}
