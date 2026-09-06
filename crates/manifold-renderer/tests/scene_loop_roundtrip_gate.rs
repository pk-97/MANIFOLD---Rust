//! SCENE_LOOP_DESIGN P4 round-trip gate (D6-migrated to the generic
//! scene-modifier pair; D6 whitelist + D11 stamping idempotence + BUG-gsql
//! framing rows), corridor-revised (ENDLESS_CORRIDOR D3/INV-EC5):
//! apply (REAL descriptor plan builder + REAL generic command) → save V1 →
//! reload → run the load-time migration the app runs (scene-object wires +
//! scene exposures + the pre-switch loop migration + the fixed-row →
//! corridor migration) → reconcile param manifests → assert the "Scene
//! Loop" section rows are EXACTLY the performer whitelist (Bars, Pattern,
//! the loop_camera framing + movement rows, Spacing, Jitter), values
//! intact, zero duplicate binding targets, every binding resolves to a
//! live node, and a performer-edited row value survives the round trip.
//! INV-EC5 adds the migration suite: pre-corridor fixtures (P1-era, P4
//! card-written, hand-desynced) load through migrate_fixed_row_scene_loops
//! → trace finds all three atoms → exposures exactly the new whitelist →
//! zero count/jitter_period/stride hits → bindings/mappings/aliases
//! dangling-reference inventory green.
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
/// framing rows, corridor-renamed (ENDLESS_CORRIDOR D3), plus the two
/// internal consumers of shared Pattern/Spacing slots. The ONLY binding
/// targets the Scene Loop section may carry. (The Bars row targets the
/// beat_ramp's bars param: with bars > 0 the ramp runs at 1/bars
/// cycles/beat, so the row reads and writes bars directly — rate = 1/bars
/// by construction.)
const WHITELIST: &[(&str, &str)] = &[
    ("loop_camera", "pattern_length"),
    ("scene_array", "cell_size"),
    ("loop_phase", "bars"),
    ("scene_array", "pattern_length"),
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
    ("loop_camera", "patterns_per_loop"),
    ("loop_camera", "sway_amp"),
    ("loop_camera", "sway_cycles"),
    ("loop_camera", "look_sweep_amp"),
    ("loop_camera", "zoom_pulse_amp"),
    ("loop_camera", "cell_size"),
    ("scene_array", "jitter_amount"),
];

fn assert_whitelist(graph: &EffectGraphDef, context: &str) {
    let meta = graph.preset_metadata.as_ref().expect("metadata");
    assert_eq!(meta.params.iter().filter(|p| p.section.as_deref() == Some("Scene Loop")).count(),
        19, "{context}: shared consumers must not mint additional card rows");
    for (source_node, target_node, param) in [
        ("scene_array", "loop_camera", "pattern_length"),
        ("loop_camera", "scene_array", "cell_size"),
    ] {
        let binding_for = |node: &str| meta.bindings.iter().find(|b| matches!(&b.target,
            manifold_core::effect_graph_def::BindingTarget::Node { node_id, param: p }
                if node_id.as_str() == node && p == param)).expect("shared binding");
        let source = binding_for(source_node);
        let consumer = binding_for(target_node);
        let mut expected = source.clone();
        expected.target = consumer.target.clone();
        expected.label = consumer.label.clone();
        assert_eq!(*consumer, expected, "{context}: shared consumer preserves slot and mapping");
    }
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

    // Pattern row default: seeded from the node's stamped pattern_length
    // (the corridor mint: 1 = uniform cells).
    let meta = graph.preset_metadata.as_ref().unwrap();
    let pattern_spec = meta
        .params
        .iter()
        .find(|p| p.section.as_deref() == Some("Scene Loop") && p.name == "Pattern")
        .expect("Pattern row stamped");
    assert_eq!(pattern_spec.default_value, 1.0);

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

    // Simulate a performer edit: Pattern 1 → 4 through the instance
    // manifest (the bound-row write path), as the panel's row would.
    let pattern_binding_id = meta
        .bindings
        .iter()
        .find(|b| {
            matches!(
                &b.target,
                manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                    if node_id.as_str() == "scene_array" && param == "pattern_length"
            )
        })
        .expect("Pattern binding")
        .id
        .clone();
    project
        .with_preset_graph_mut(
            &manifold_core::GraphTarget::Generator(layer_id.clone()),
            |inst| inst.set_base_param(&pattern_binding_id, 4.0),
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
    // migration + the pre-switch loop migration (D8) + the fixed-row →
    // corridor migration (ENDLESS_CORRIDOR D7, BEFORE the exposure rows)
    // + the P4 row enrichment migration, then the manifest reconcile.
    for layer in &mut reloaded.timeline.layers {
        if let Some(graph) = layer.gen_params_mut().and_then(|gp| gp.graph.as_mut()) {
            manifold_core::scene_object_migration::migrate_scene_object_wires(graph);
            manifold_renderer::node_graph::scene_exposure::migrate_scene_exposures(graph);
            manifold_renderer::node_graph::scene_modifier::migrate_pre_switch_scene_loops(graph);
            assert!(
                !manifold_renderer::node_graph::scene_modifier::migrate_fixed_row_scene_loops(graph),
                "a corridor-minted graph is new-shape — the migration is a no-op"
            );
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

    // The performer's Pattern edit survived: the reloaded instance manifest
    // still carries the row and its edited base value (reconcile must SEE
    // the stamped entries and keep them — no "no template descriptor, no
    // inline spec" drops).
    let pattern_id = {
        let meta = reloaded_graph.preset_metadata.as_ref().unwrap();
        meta
            .bindings
            .iter()
            .find(|b| {
                matches!(
                    &b.target,
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                        if node_id.as_str() == "scene_array" && param == "pattern_length"
                )
            })
            .expect("Pattern binding kept after reload")
            .id
            .clone()
    };
    let base = reloaded
        .with_preset_graph_mut(&manifold_core::GraphTarget::Generator(layer_id.clone()), |inst| {
            inst.params
                .contains(pattern_id.as_str())
                .then(|| inst.get_base_param(pattern_id.as_str()))
        })
        .flatten()
        .expect("instance param kept");
    assert_eq!(base, 4.0, "performer edit survives save/reload/reconcile");

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
    let _ = manifold_renderer::node_graph::scene_modifier::migrate_fixed_row_scene_loops(&mut def);
    let _ = manifold_renderer::node_graph::scene_modifier::migrate_loop_exposure_rows(&mut def);

    assert_whitelist(&def, "after flatten renumber + migration");
}

// ---------------------------------------------------------------------------
// INV-EC5 — the fixed-row → corridor load migration (ENDLESS_CORRIDOR D7)
// ---------------------------------------------------------------------------

use manifold_core::effect_graph_def::{AliasEntry, BindingTarget, ParamSpecDef, ValueAliasEntry};

/// The old-ids surface a saved P4 project carries: returned by the
/// downgrade helper so the inventory tests can assert preservation.
struct OldRowIds {
    copies_id: String,
    stride_id: String,
    jitter_user_id: String,
}

/// Downgrade a fresh corridor-minted applied loop to the saved P4 fixed-row
/// shape: scene_array gains count/jitter_period (loses pattern_length),
/// loop_camera gains stride (loses patterns_per_loop/pattern_length), the
/// camera wire is dropped, the stamped rows retarget to the old params with
/// their ORIGINAL P4 ids ("{doc}_count" / "{doc}_stride"), a user-added
/// jitter_period exposure lands (the graph-editor expose checkbox), and
/// alias tables name the old spec ids. `j`/`s` None = the param is absent
/// entirely (the P1-era save shape).
fn downgrade_to_pre_corridor(def: &mut EffectGraphDef, j: Option<f32>, s: Option<f32>, count: f32) -> OldRowIds {
    let array_doc = def
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "scene_array")
        .map(|n| n.id)
        .expect("scene_array");
    let camera_doc = def
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_camera")
        .map(|n| n.id)
        .expect("loop_camera");

    let array = def.nodes.iter_mut().find(|n| n.id == array_doc).unwrap();
    array.params.remove("pattern_length");
    array.params.insert(
        "count".to_string(),
        SerializedParamValue::Float { value: count },
    );
    if let Some(j) = j {
        array.params.insert(
            "jitter_period".to_string(),
            SerializedParamValue::Float { value: j },
        );
    }
    let camera = def.nodes.iter_mut().find(|n| n.id == camera_doc).unwrap();
    camera.params.remove("patterns_per_loop");
    camera.params.remove("pattern_length");
    if let Some(s) = s {
        camera.params.insert(
            "stride".to_string(),
            SerializedParamValue::Float { value: s },
        );
    }

    // The P4 save has no scene_array camera wire.
    def.wires
        .retain(|w| !(w.from_node == camera_doc && w.to_node == array_doc && w.to_port == "camera"));

    // Exposure rows: retarget to the old params with the ids a P4 save
    // carried, plus the user-added jitter row and the alias entries.
    let copies_id = format!("{array_doc}_count");
    let stride_id = format!("{camera_doc}_stride");
    let jitter_user_id = "scene_array_jitter_period_user".to_string();
    let meta = def.preset_metadata.as_mut().unwrap();
    // Fixed-row saves predate shared consumers; remove those new bindings
    // before rewriting the exposed rows into their historical shape.
    meta.bindings.retain(|b| !matches!(&b.target,
        BindingTarget::Node { node_id, param }
            if (node_id.as_str() == "loop_camera" && param == "pattern_length")
                || (node_id.as_str() == "scene_array" && param == "cell_size")));
    for b in &mut meta.bindings {
        match &mut b.target {
            BindingTarget::Node { node_id, param }
                if node_id.as_str() == "scene_array" && param == "pattern_length" =>
            {
                *param = "count".to_string();
                b.id = copies_id.clone();
                b.label = "Copies".to_string();
            }
            BindingTarget::Node { node_id, param }
                if node_id.as_str() == "loop_camera" && param == "patterns_per_loop" =>
            {
                *param = "stride".to_string();
                b.id = stride_id.clone();
            }
            _ => {}
        }
    }
    for p in &mut meta.params {
        if p.id == format!("{array_doc}_pattern_length") {
            p.id = copies_id.clone();
            p.name = "Copies".to_string();
            p.default_value = count;
        } else if p.id == format!("{camera_doc}_patterns_per_loop") {
            p.id = stride_id.clone();
            p.default_value = s.unwrap_or(1.0);
        }
    }
    // The user-added jitter row: a cloned stamped binding/spec flipped to
    // user_added with the jitter_period target (the graph-editor "expose"
    // checkbox a performer could have used on the internal param).
    let mut user_binding = meta
        .bindings
        .iter()
        .find(|b| b.id == copies_id)
        .expect("copies binding")
        .clone();
    user_binding.id = jitter_user_id.clone();
    user_binding.label = "Jitter Period".to_string();
    user_binding.target = BindingTarget::Node {
        node_id: manifold_core::NodeId::new("scene_array"),
        param: "jitter_period".to_string(),
    };
    user_binding.user_added = true;
    meta.bindings.push(user_binding);
    meta.params.push(ParamSpecDef {
        id: jitter_user_id.clone(),
        name: "Jitter Period".to_string(),
        min: 1.0,
        max: 8.0,
        default_value: j.unwrap_or(1.0),
        whole_numbers: true,
        section: Some("Scene Loop".to_string()),
        ..Default::default()
    });
    // Saved alias tables naming the old spec ids.
    meta.param_aliases.push(AliasEntry {
        old: "copies_legacy".to_string(),
        new: Some(copies_id.clone()),
    });
    meta.value_aliases.push(ValueAliasEntry {
        param_id: stride_id.clone(),
        mapping: vec![(1, 2)],
    });

    OldRowIds { copies_id, stride_id, jitter_user_id }
}

/// The app's load order for a saved loop: the corridor migration, then the
/// exposure-row migration (D7 order — the jitter_period re-stamp inside it
/// is gated on the old node shape the corridor migration removes).
fn run_load_migrations(def: &mut EffectGraphDef) {
    assert!(
        manifold_renderer::node_graph::scene_modifier::migrate_fixed_row_scene_loops(def),
        "a pre-corridor fixture must migrate"
    );
    let _ = manifold_renderer::node_graph::scene_modifier::migrate_loop_exposure_rows(def);
}

/// INV-EC5 shared assertions: the migrated def is a corridor loop with the
/// D7 arithmetic, zero old-param hits, and the new whitelist.
fn assert_migrated_corridor(
    def: &EffectGraphDef,
    expected_j: f32,
    expected_patterns: f32,
    context: &str,
) {
    use manifold_renderer::node_graph::scene_modifier::{SCENE_LOOP_DESCRIPTOR, trace_modifier};
    let result = trace_modifier(&SCENE_LOOP_DESCRIPTOR, &def.nodes);
    assert!(result.applied(&SCENE_LOOP_DESCRIPTOR), "{context}: the three atoms trace after migration");
    let array_doc = result.doc_ids["scene_array"];
    let camera_doc = result.doc_ids["loop_camera"];

    // D2 wire: the corridor window derives from the loop camera.
    assert!(
        def.wires
            .iter()
            .any(|w| w.from_node == camera_doc && w.to_node == array_doc && w.to_port == "camera"),
        "{context}: loop_camera.out → scene_array.camera must exist after migration"
    );

    // D7 arithmetic.
    let param = |doc: u32, name: &str| {
        def.nodes
            .iter()
            .find(|n| n.id == doc)
            .and_then(|n| n.params.get(name))
            .cloned()
    };
    assert_eq!(
        param(array_doc, "pattern_length"),
        Some(SerializedParamValue::Float { value: expected_j }),
        "{context}: scene_array.pattern_length = J"
    );
    assert_eq!(
        param(camera_doc, "pattern_length"),
        Some(SerializedParamValue::Float { value: expected_j }),
        "{context}: loop_camera.pattern_length = J"
    );
    assert_eq!(
        param(camera_doc, "patterns_per_loop"),
        Some(SerializedParamValue::Float { value: expected_patterns }),
        "{context}: loop_camera.patterns_per_loop = round(S/J)"
    );

    // Negative gate (INV-EC5): zero count/jitter_period/stride param hits
    // in the migrated def — nodes and exposure bindings both.
    for n in &def.nodes {
        for dead in ["count", "jitter_period", "stride"] {
            assert!(
                !n.params.contains_key(dead),
                "{context}: migrated node {} still carries {dead}",
                n.node_id
            );
        }
    }
    let meta = def.preset_metadata.as_ref().expect("metadata");
    for b in &meta.bindings {
        if let BindingTarget::Node { param, .. } = &b.target {
            assert!(
                !matches!(param.as_str(), "count" | "jitter_period" | "stride"),
                "{context}: a binding still targets the dropped param {param}"
            );
        }
    }

    // Post-migration purity: travel = K·P cells ≡ 0 (mod P) for any
    // integers (D3) — and the D7 gate held: the exposure migration did
    // NOT re-insert the deleted jitter_period.
    let p = expected_j as i32;
    let k = expected_patterns as i32;
    assert_eq!(
        (k * p) % p.max(1),
        0,
        "{context}: the migrated loop must be wrap-pure by construction"
    );
    assert!(
        !def.nodes
            .iter()
            .find(|n| n.id == array_doc)
            .unwrap()
            .params
            .contains_key("jitter_period"),
        "{context}: the exposure migration's jitter_period re-stamp must stay gated off the new shape (D7)"
    );

    assert_whitelist(def, context);
}

/// INV-EC5 case 1: a P1-era save (count, NO stride/jitter_period) migrates
/// to the uniform corridor at J = 1, K = 1.
#[test]
fn inv_ec5_p1_era_fixed_row_loop_migrates() {
    let mut project = Project::default();
    let (_layer_id, idx) = apply_loop(&mut project, grouped_scene_def());
    let graph = project.timeline.layers[idx]
        .gen_params_mut()
        .and_then(|gp| gp.graph.as_mut())
        .expect("graph");
    downgrade_to_pre_corridor(graph, None, None, 3.0);
    let mut def = graph.clone();

    run_load_migrations(&mut def);
    assert_migrated_corridor(&def, 1.0, 1.0, "P1-era");

    // Idempotent: the migrated graph is new-shape, a second run is a no-op.
    assert!(
        !manifold_renderer::node_graph::scene_modifier::migrate_fixed_row_scene_loops(&mut def),
        "INV-EC5: the migration is idempotent"
    );
}

/// INV-EC5 case 2: a P4 card-written save (the shipped coupling wrote
/// J = S) migrates with travel preserved: S = 4, J = 4 → K = 1, P = 4,
/// four cells per loop before and after.
#[test]
fn inv_ec5_p4_card_written_loop_migrates() {
    let mut project = Project::default();
    let (_layer_id, idx) = apply_loop(&mut project, grouped_scene_def());
    let graph = project.timeline.layers[idx]
        .gen_params_mut()
        .and_then(|gp| gp.graph.as_mut())
        .expect("graph");
    downgrade_to_pre_corridor(graph, Some(4.0), Some(4.0), 6.0);
    let mut def = graph.clone();

    run_load_migrations(&mut def);
    assert_migrated_corridor(&def, 4.0, 1.0, "P4 card-written");
}

/// INV-EC5 case 3: a hand-desynced save (S = 7, J = 3 — a graph-editor
/// edit the shipped coupling never wrote). The D7 ruling: the migration
/// ALWAYS lands pure, travel may change — 7 cells → round(7/3)·3 = 6.
#[test]
fn inv_ec5_hand_desynced_loop_migrates_pure_with_travel_shift() {
    let mut project = Project::default();
    let (_layer_id, idx) = apply_loop(&mut project, grouped_scene_def());
    let graph = project.timeline.layers[idx]
        .gen_params_mut()
        .and_then(|gp| gp.graph.as_mut())
        .expect("graph");
    downgrade_to_pre_corridor(graph, Some(3.0), Some(7.0), 8.0);
    let mut def = graph.clone();

    // The fixture is genuinely impure pre-migration: 7 mod 3 ≠ 0.
    let (s, j) = (7.0f32, 3.0f32);
    assert_ne!((s as i32) % (j as i32), 0, "fixture: the hand-desynced loop must start impure");

    run_load_migrations(&mut def);
    assert_migrated_corridor(&def, 3.0, 2.0, "hand-desynced S=7 J=3");

    // The travel changed 7 → 6 cells and the result is pure (asserted in
    // assert_migrated_corridor): 2·3 ≡ 0 (mod 3).
    let camera = def
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_camera")
        .expect("loop_camera");
    let k = match camera.params.get("patterns_per_loop") {
        Some(SerializedParamValue::Float { value }) => *value,
        _ => panic!("patterns_per_loop missing"),
    };
    let p = match camera.params.get("pattern_length") {
        Some(SerializedParamValue::Float { value }) => *value,
        _ => panic!("pattern_length missing"),
    };
    assert_eq!(
        k * p, 6.0,
        "hand-desynced: travel shifts 7 → 6 cells per the D7 ruling"
    );
}

/// INV-EC5 + the binding ruling (Peter 2026-09-06): the dangling-reference
/// inventory across a migrated save — user_added exposure entries,
/// instance-level base values / drivers / envelopes / Ableton mappings
/// keyed by the old spec ids, and param/value alias entries. Every
/// reference must survive the migration with its id preserved (retargeted
/// to the new param names), and nothing may dangle.
#[test]
fn inv_ec5_binding_rewrite_inventory_stays_live() {
    let mut project = Project::default();
    let (layer_id, idx) = apply_loop(&mut project, grouped_scene_def());
    let old = {
        let graph = project.timeline.layers[idx]
            .gen_params_mut()
            .and_then(|gp| gp.graph.as_mut())
            .expect("graph");
        downgrade_to_pre_corridor(graph, Some(2.0), Some(2.0), 4.0)
    };
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());

    // A real P4 save carried the instance manifest keyed by the P4 spec
    // ids — seed those entries directly (set_base_param only resolves ids
    // the manifest already knows, and this fixture has no P4 build history
    // to reconcile from).
    let p4_param = |id: &str, name: &str, value: f32| {
        manifold_core::params::Param::bundled(ParamSpecDef {
            id: id.to_string(),
            name: name.to_string(),
            min: 1.0,
            max: 8.0,
            default_value: value,
            whole_numbers: true,
            ..Default::default()
        })
    };
    project
        .with_preset_graph_mut(&target, |inst| {
            inst.params.insert_at(usize::MAX, p4_param(&old.copies_id, "Copies", 5.0));
            inst.params.insert_at(usize::MAX, p4_param(&old.stride_id, "Stride", 2.0));
            inst.drivers = Some(vec![manifold_core::effects::ParameterDriver {
                param_id: std::borrow::Cow::Owned(old.copies_id.clone()),
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
                old.stride_id.clone(),
            )]);
            inst.ableton_mappings = Some(vec![manifold_core::ableton_mapping::AbletonParamMapping {
                param_id: std::borrow::Cow::Owned(old.stride_id.clone()),
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

    // Migrate (the app's load sequence).
    let mut def = project.timeline.layers[idx]
        .generator_graph()
        .expect("graph")
        .clone();
    run_load_migrations(&mut def);

    // (a) The renamed rows: SAME ids, new targets, new labels. The
    // user-added jitter_period row is gone entirely.
    let meta = def.preset_metadata.as_ref().expect("metadata");
    let binding = |id: &str| meta.bindings.iter().find(|b| b.id == id);
    assert!(
        matches!(
            binding(&old.copies_id).map(|b| &b.target),
            Some(BindingTarget::Node { node_id, param })
                if node_id.as_str() == "scene_array" && param == "pattern_length"
        ),
        "the Copies row keeps its id and retargets to (scene_array, pattern_length)"
    );
    assert!(
        matches!(
            binding(&old.stride_id).map(|b| &b.target),
            Some(BindingTarget::Node { node_id, param })
                if node_id.as_str() == "loop_camera" && param == "patterns_per_loop"
        ),
        "the Stride row keeps its id and retargets to (loop_camera, patterns_per_loop)"
    );
    assert!(binding(&old.jitter_user_id).is_none(), "the user-added jitter_period row drops (no successor param)");
    assert!(
        !meta.params.iter().any(|p| p.id == old.jitter_user_id),
        "the user-added jitter_period spec drops with its binding"
    );
    let spec = |id: &str| meta.params.iter().find(|p| p.id == id);
    assert_eq!(
        spec(&old.copies_id).map(|p| p.name.as_str()),
        Some("Pattern"),
        "the retargeted row carries the Pattern label"
    );
    assert_eq!(
        spec(&old.copies_id).map(|p| p.default_value),
        Some(2.0),
        "the Pattern row's default rides the migrated J value"
    );
    assert!(
        meta.bindings.iter().filter(|b| b.user_added).all(|b| {
            !matches!(
                &b.target,
                BindingTarget::Node { param, .. }
                    if matches!(param.as_str(), "count" | "jitter_period" | "stride")
            )
        }),
        "(a) no user_added exposure entry targets a dropped param"
    );

    // (b) Saved mappings: every instance-level reference keyed by the old
    // ids still resolves to a live spec id after the migration.
    let live_id = |id: &str| -> bool { meta.params.iter().any(|p| p.id == id) };
    assert!(live_id(&old.copies_id) && live_id(&old.stride_id), "old ids remain live specs");
    let base = project
        .with_preset_graph_mut(&target, |inst| inst.get_base_param(old.copies_id.as_str()))
        .expect("base value reachable");
    assert_eq!(base, 5.0, "the performer's Copies base value rides its preserved id");
    let refs_resolve = project
        .with_preset_graph_mut(&target, |inst| {
            let drivers_ok = inst.drivers.as_ref().map(|ds| {
                ds.iter().all(|d| live_id(d.param_id.as_ref()))
            }).unwrap_or(true);
            let envelopes_ok = inst.envelopes.as_ref().map(|es| {
                es.iter().all(|e| live_id(e.param_id.as_ref()))
            }).unwrap_or(true);
            let ableton_ok = inst.ableton_mappings.as_ref().map(|ms| {
                ms.iter().all(|m| live_id(m.param_id.as_ref()))
            }).unwrap_or(true);
            drivers_ok && envelopes_ok && ableton_ok
        })
        .expect("instance reachable");
    assert!(refs_resolve, "(b) drivers/envelopes/Ableton mappings reference only live spec ids");

    // (c) Alias tables: every aliased id resolves to a live spec.
    for a in &meta.param_aliases {
        assert!(
            a.new.as_ref().map(|n| live_id(n)).unwrap_or(true),
            "(c) param_aliases entry {} dangles", a.old
        );
    }
    for v in &meta.value_aliases {
        assert!(
            live_id(&v.param_id),
            "(c) value_aliases entry for {} dangles", v.param_id
        );
    }
}
