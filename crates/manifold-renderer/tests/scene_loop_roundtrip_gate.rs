//! SCENE_LOOP_DESIGN P4 round-trip gate (D6-migrated to the generic
//! scene-modifier pair; D6 whitelist + D11 stamping idempotence + BUG-gsql
//! framing rows):
//! apply (REAL descriptor plan builder + REAL generic command) → save V1 →
//! reload → run the load-time migration the app runs (scene-object wires +
//! scene exposures + the pre-switch loop migration) → reconcile param
//! manifests → assert the "Scene Loop" section rows are EXACTLY the
//! performer whitelist (Bars, Copies, the loop_camera framing + movement
//! rows, Spacing, Jitter), values intact, zero duplicate binding targets,
//! every binding resolves to a live node, and a performer-edited row value
//! survives the round trip.
//!
//! Fails on the pre-P4 code: the apply stamped EVERY param of EVERY loop
//! node, so the section carried duplicate Axis / Cell Size rows (one set
//! from scene_array, one from loop_camera) plus the atoms' internals
//! (attack, home, near, far, fov_y, fog) — the desync Peter hit.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, PresetMetadata, SerializedParamValue,
};
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::ApplySceneModifierCommand;
use manifold_renderer::node_graph::scene_modifier::{build_plan, LOOP_KIND_ID};

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
/// D4 gap rule).
fn grouped_scene_def() -> EffectGraphDef {
    EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::from_string("LoopGateScene".to_string()),
            display_name: "Loop Gate Scene".to_string(),
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
            node(2, "render", "node.render_scene", BTreeMap::new()),
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

fn apply_loop(project: &mut Project, def: EffectGraphDef) -> (manifold_foundation::LayerId, usize) {
    let idx = project.timeline.add_layer(
        "Loop Gate",
        LayerType::Generator,
        PresetTypeId::from_string("LoopGateScene".to_string()),
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(def);
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let render_scene_id = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .nodes
        .iter()
        .find(|n| n.type_id == "node.render_scene")
        .expect("render_scene")
        .id;
    // The REAL descriptor plan builder (D1) — the same registry dispatch the
    // panel's "Enable Scene Loop" uses.
    let plan = build_plan(
        LOOP_KIND_ID,
        project.timeline.layers[idx].generator_graph().expect("graph"),
        render_scene_id,
    )
    .expect("plan builder succeeds on the grouped scene");
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let catalog = EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let mut cmd = ApplySceneModifierCommand::new(target, Vec::new(), plan, catalog);
    cmd.execute(project);
    (layer_id, idx)
}

/// (node_id, param) targets of every Scene Loop section binding, in stamp
/// order — the panel-visible surface.
fn scene_loop_targets(graph: &EffectGraphDef) -> Vec<(String, String)> {
    let meta = graph.preset_metadata.as_ref().expect("preset metadata");
    let section_ids: std::collections::BTreeSet<&str> = meta
        .params
        .iter()
        .filter(|p| p.section.as_deref() == Some("Scene Loop"))
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

/// D6 P4 whitelist + SCENE_MODIFIER_FRAMEWORK P4 enrichment + BUG-gsql
/// framing rows — the ONLY binding targets the Scene Loop section may
/// carry. (The Bars row targets the beat_ramp's bars param: with bars > 0
/// the ramp runs at 1/bars cycles/beat, so the row reads and writes bars
/// directly — rate = 1/bars by construction.)
const WHITELIST: &[(&str, &str)] = &[
    ("loop_phase", "bars"),
    ("scene_array", "count"),
    ("loop_camera", "height"),
    ("loop_camera", "lateral"),
    ("loop_camera", "near"),
    ("loop_camera", "far"),
    ("loop_camera", "fov_y"),
    ("loop_camera", "home"),
    ("loop_camera", "roll"),
    ("loop_camera", "pitch"),
    ("loop_camera", "yaw"),
    ("loop_camera", "flow"),
    ("loop_camera", "stride"),
    ("loop_camera", "sway_amp"),
    ("loop_camera", "sway_cycles"),
    ("loop_camera", "look_sweep_amp"),
    ("loop_camera", "zoom_pulse_amp"),
    ("loop_camera", "cell_size"),
    ("scene_array", "jitter_amount"),
];

fn assert_whitelist(graph: &EffectGraphDef, context: &str) {
    let mut targets = scene_loop_targets(graph);
    targets.sort();
    let mut expected: Vec<(String, String)> = WHITELIST
        .iter()
        .map(|(n, p)| (n.to_string(), p.to_string()))
        .collect();
    expected.sort();
    assert_eq!(
        targets, expected,
        "{context}: Scene Loop section must be exactly the D6 whitelist"
    );

    // Zero duplicates: no two section rows may target the same (node, param).
    let mut seen = std::collections::BTreeSet::new();
    for t in &targets {
        assert!(seen.insert(t.clone()), "{context}: duplicate row {t:?}");
    }

    // Every section binding must resolve to a live node param.
    for (node_id, param) in &targets {
        let found = graph.nodes.iter().any(|n| {
            n.node_id.as_str() == node_id
                && manifold_renderer::node_graph::scene_exposure::metadata_for_node_type(&n.type_id)
                    .iter()
                    .any(|m| m.name == *param)
        });
        assert!(found, "{context}: Scene Loop row ({node_id}, {param}) has no live node");
    }
}

/// The P4 round-trip gate: apply → save → reload → migrate + reconcile →
/// the stamped rows are exactly the whitelist, values unchanged, no
/// duplicates, performer edit survives.
#[test]
fn scene_loop_roundtrip_whitelist_rows_stable() {
    let mut project = Project::default();
    let def = grouped_scene_def();
    let expected_cell = 10.0_f32; // 2 × the 5-unit Z extent (D4 gap rule)
    let (layer_id, idx) = apply_loop(&mut project, def);

    let graph = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph after apply")
        .clone();
    assert_whitelist(&graph, "after apply");

    // Bars row: the minted loop_phase must carry bars = 8 (D10 default),
    // governing rate = 1/bars.
    let loop_phase = graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_phase")
        .expect("loop_phase minted");
    assert_eq!(
        loop_phase.params.get("bars"),
        Some(&SerializedParamValue::Float { value: 8.0 }),
        "loop_phase minted with bars = 8 (rate = 1/bars by construction)"
    );

    // Copies row default: seeded from the node's stamped count (D10: 3).
    let meta = graph.preset_metadata.as_ref().unwrap();
    let copies_spec = meta
        .params
        .iter()
        .find(|p| p.section.as_deref() == Some("Scene Loop") && p.name == "Copies")
        .expect("Copies row stamped");
    assert_eq!(copies_spec.default_value, 3.0);

    // cell_size feeds scene_array AND loop_camera from the one plan-builder
    // value (INV-4); P4 made it the Spacing row — both nodes still carry the
    // plan value, and the row's stamped range is the curated auto×0.25..4.0
    // band, not the manifest's generic 0.01..1000.
    for node_id in ["scene_array", "loop_camera"] {
        let n = graph.nodes.iter().find(|n| n.node_id.as_str() == node_id).unwrap();
        assert_eq!(
            n.params.get("cell_size"),
            Some(&SerializedParamValue::Float { value: expected_cell }),
            "INV-4: {node_id} cell_size = plan value"
        );
    }
    let spacing_spec = meta
        .params
        .iter()
        .find(|p| p.section.as_deref() == Some("Scene Loop") && p.name == "Spacing")
        .expect("Spacing row stamped");
    assert_eq!(
        (spacing_spec.min, spacing_spec.max),
        (expected_cell * 0.25, expected_cell * 4.0),
        "Spacing range curated to auto×0.25..4.0"
    );
    assert_eq!(spacing_spec.default_value, expected_cell);

    // BUG-gsql framing rows: Near/Far/Home stamped with the cell-scaled
    // bands (not the manifests' room-scale generics), defaults at the
    // plan-minted values (home = −cell/2, near = 0.002·cell, far = 4·cell).
    // The Roll/Pitch/Yaw angle rows carry the manifest band (±3.2) and
    // default 0 — the primitive's rotate_local no-op.
    let section_spec = |graph: &EffectGraphDef, name: &str| {
        graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .find(|p| p.section.as_deref() == Some("Scene Loop") && p.name == name)
            .unwrap_or_else(|| panic!("{name} row stamped"))
            .clone()
    };
    let near_spec = section_spec(&graph, "Near");
    assert_eq!(
        (near_spec.min, near_spec.max),
        (0.001, expected_cell * 2.0),
        "Near range curated to the cell band"
    );
    assert_eq!(near_spec.default_value, expected_cell * 0.002);
    let far_spec = section_spec(&graph, "Far");
    assert_eq!(
        (far_spec.min, far_spec.max),
        (1.0, (expected_cell * 20.0).min(10_000.0)),
        "Far range curated to the cell band"
    );
    assert_eq!(far_spec.default_value, expected_cell * 4.0);
    let home_spec = section_spec(&graph, "Home");
    assert_eq!(
        (home_spec.min, home_spec.max),
        (-expected_cell * 2.0, expected_cell * 2.0),
        "Home range curated to ±2 cells"
    );
    assert_eq!(home_spec.default_value, -expected_cell * 0.5);
    for angle in ["Roll", "Pitch", "Yaw"] {
        let spec = section_spec(&graph, angle);
        assert_eq!(
            (spec.min, spec.max, spec.default_value),
            (-3.2, 3.2, 0.0),
            "{angle} row carries the manifest angle band"
        );
    }

    // Simulate a performer edit: Copies 3 → 5 through the instance manifest
    // (the bound-row write path), as the panel's row would.
    let copies_binding_id = meta
        .bindings
        .iter()
        .find(|b| {
            matches!(
                &b.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                    if node_id.as_str() == "scene_array" && param == "count"
            )
        })
        .expect("Copies binding")
        .id
        .clone();
    project
        .with_preset_graph_mut(
            &manifold_core::GraphTarget::Generator(layer_id.clone()),
            |inst| inst.set_base_param(&copies_binding_id, 5.0),
        )
        .expect("instance reachable");

    // Save → reload → the app's load migration + reconcile.
    let path = std::env::temp_dir().join(format!(
        "manifold_scene_loop_gate_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &path).expect("save v1");
    let mut reloaded = manifold_io::loader::load_project(&path).expect("load v1");

    // The project_io.rs load path: per-layer wire migration + exposure
    // migration + the pre-switch loop migration (D8) + the P4 row
    // enrichment migration, then the manifest reconcile.
    for layer in &mut reloaded.timeline.layers {
        if let Some(graph) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) {
            manifold_core::scene_object_migration::migrate_scene_object_wires(graph);
            manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(graph);
            manifold_renderer::node_graph::scene_modifier::migrate_pre_switch_scene_loops(graph);
            assert!(
                !manifold_renderer::node_graph::scene_modifier::migrate_loop_exposure_rows(graph),
                "rows stamped at apply must all be present at reload — migration is a no-op"
            );
        }
    }
    reloaded.reconcile_param_manifests();

    let reloaded_graph = reloaded
        .timeline
        .layers
        .iter()
        .find(|l| l.layer_id == layer_id)
        .expect("layer survived")
        .generator_graph()
        .expect("graph override survived reload");
    assert_whitelist(reloaded_graph, "after reload");

    // Values intact: the migrated def's loop nodes carry the same params.
    let reloaded_phase = reloaded_graph
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_phase")
        .expect("loop_phase after reload");
    assert_eq!(
        reloaded_phase.params.get("bars"),
        Some(&SerializedParamValue::Float { value: 8.0 }),
        "bars value survives the round trip"
    );

    // The performer's Copies edit survived: the reloaded instance manifest
    // still carries the row and its edited base value (reconcile must SEE
    // the stamped entries and keep them — no "no template descriptor, no
    // inline spec" drops).
    let copies_id = {
        let meta = reloaded_graph.preset_metadata.as_ref().unwrap();
        meta
            .bindings
            .iter()
            .find(|b| {
                matches!(
                    &b.target,
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                        if node_id.as_str() == "scene_array" && param == "count"
                )
            })
            .expect("Copies binding kept after reload")
            .id
            .clone()
    };
    let base = reloaded
        .with_preset_graph_mut(&manifold_core::GraphTarget::Generator(layer_id.clone()), |inst| {
            inst.params
                .contains(copies_id.as_str())
                .then(|| inst.get_base_param(copies_id.as_str()))
        })
        .flatten()
        .expect("instance param kept");
    assert_eq!(base, 5.0, "performer edit survives save/reload/reconcile");

    let _ = std::fs::remove_file(&path);
}

/// D11: the load-migration stamper must match existing exposures by BINDING
/// TARGET (nodeId, param) — a def whose doc ids were renumbered by
/// flattening must not mint a second set of Scene Loop rows.
#[test]
fn scene_loop_renumber_after_flatten_mints_no_second_exposure() {
    let mut project = Project::default();
    let (_layer_id, idx) = apply_loop(&mut project, grouped_scene_def());

    // Replace the layer graph with its flattened self: flatten renumbers
    // every doc id fresh and copies preset_metadata verbatim — the stamped
    // "{doc_id}_{param}" ids no longer match any live doc id.
    let applied = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .clone();
    let flat = manifold_core::flatten::flatten_groups(&applied).expect("flatten");
    assert!(
        flat.nodes.iter().map(|n| n.id).collect::<std::collections::BTreeSet<_>>()
            != applied.nodes.iter().map(|n| n.id).collect::<std::collections::BTreeSet<_>>(),
        "the fixture must actually renumber (groups present)"
    );
    project.timeline.layers[idx].gen_params_or_init().graph = Some(flat.clone());

    // Load migration on the renumbered def — must not mint a second
    // exposure set for the renumbered doc ids (D11: idempotence by binding
    // target, not by stamped id).
    let mut def = flat;
    manifold_core::scene_object_migration::migrate_scene_object_wires(&mut def);
    let _ = manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(&mut def);
    let _ = manifold_renderer::node_graph::scene_modifier::migrate_pre_switch_scene_loops(&mut def);
    let _ = manifold_renderer::node_graph::scene_modifier::migrate_loop_exposure_rows(&mut def);

    assert_whitelist(&def, "after flatten renumber + migration");
}
