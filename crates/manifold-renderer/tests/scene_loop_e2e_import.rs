//! SCENE_LOOP_DESIGN P2 end-to-end gate (D6-migrated): drive the REAL
//! renderer-side plan builder (the `scene_loop` modifier descriptor's
//! `plan_builder`, SCENE_MODIFIER_FRAMEWORK D1) through the REAL generic
//! editing command (`ApplySceneModifierCommand`) against a REAL imported
//! GLB graph (`assemble_import_graph` on
//! `tests/fixtures/gltf/apricot_tl05.glb`) and assert the applied graph's
//! structural facts.
//!
//! This is the seam that let P1 ship: a hand-built plan in a unit test never
//! exercised production plan construction. Here the plan comes from the
//! SAME builder the panel's "Enable Scene Loop" dispatches.
//!
//! Wrap parity (INV-3) on this real-import path was attempted and DELETED
//! (P4): two frames of ONE session through ONE shared GpuDevice still differ
//! (≈80 max pixel diff) — the import's AO/cinematic path is nondeterministic
//! in-session, not just per device instance. BUG-twa6 (device-seed) tracks
//! the retirement; until it lands, INV-3 gates on the deterministic minimal
//! graph (`scene_loop_wrap_parity.rs`) and this file gates structure only.

use std::path::Path;

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_editing::command::Command;
use manifold_editing::commands::graph::ApplySceneModifierCommand;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier::{build_plan, LOOP_KIND_ID};
use manifold_renderer::node_graph::scene_vm::RENDER_SCENE_TYPE_ID;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/apricot_tl05.glb"
);

// The end-to-end gate: the REAL plan builder → REAL command → REAL import
// path, verified on the applied graph's structure AND the pipeline facts the
// splice must produce. Pixel-diff copies are proven on the hand-built
// `scene_loop_probe.rs` graphs (this GLB itself renders near-black through the
// throwaway headless runtime — a harness limitation, tracked in the verdict).
#[test]
fn scene_loop_apply_import_renders_copies() {
    let (def, report) = assemble_import_graph(Path::new(FIXTURE))
        .unwrap_or_else(|e| panic!("assemble_import_graph({FIXTURE}) failed: {e}"));
    assert!(
        report.object_count > 0,
        "fixture must import at least one object group"
    );
    let render_scene_id = def
        .nodes
        .iter()
        .find(|n| n.type_id == RENDER_SCENE_TYPE_ID)
        .expect("import has a render_scene node")
        .id;

    // The REAL plan builder (D1) — the same one the panel dispatches.
    let plan = build_plan(LOOP_KIND_ID, &def, render_scene_id)
        .expect("plan builder must succeed on the imported scene");
    assert!(
        !plan.group_splices.is_empty(),
        "plan must splice every object group's instances port"
    );

    let layer_count = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
    assert!(
        plan.new_nodes.iter().all(|n| n.id > layer_count),
        "plan mints fresh doc ids beyond the import's"
    );

    // Apply through the REAL command (editing) against a Project, exactly as
    // the panel's ProjectAction does.
    let mut project = Project::default();
    let idx = project.timeline.add_layer(
        "Apricot Loop",
        LayerType::Generator,
        PresetTypeId::from_string("ApricotLoopTest".to_string()),
    );
    {
        let layer = &mut project.timeline.layers[idx];
        layer.gen_params_or_init().graph = Some(def.clone());
    }
    let layer_id = project.timeline.layers[idx].layer_id.clone();
    let target = manifold_core::GraphTarget::Generator(layer_id);
    let catalog = manifold_core::effect_graph_def::EffectGraphDef {
        version: 1,
        name: None,
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    let mut cmd = ApplySceneModifierCommand::new(target, Vec::new(), plan, catalog);
    cmd.execute(&mut project);

    let applied = project.timeline.layers[idx]
        .generator_graph()
        .expect("layer graph survives apply");

    // End-to-end applied-graph gate on the REAL import (structural facts —
    // the pixel copies proof lives in `scene_loop_probe.rs`: this GLB renders
    // near-black through the throwaway headless runtime, so pixel assertions
    // on it would flake on the harness, not the splice).
    //
    // 1. Loop nodes minted with the D10-ruled pose: loop_camera home=-cell/2
    //    (corridor entry), scene_array cell_size matching.
    let loop_camera = applied
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_camera")
        .expect("loop_camera minted");
    let home = match loop_camera.params.get("home") {
        Some(SerializedParamValue::Float { value }) => *value,
        _ => panic!("loop_camera must carry a home param"),
    };
    let cell_size = match loop_camera.params.get("cell_size") {
        Some(SerializedParamValue::Float { value }) => *value,
        _ => panic!("loop_camera cell_size"),
    };
    assert!(
        (home + cell_size * 0.5).abs() < 1e-3,
        "loop_camera home must be -cell_size/2 (corridor entry), got home={home} cell={cell_size}"
    );

    // 2. Camera re-point through the D5 Switch enable path: loop_camera →
    //    loop_cam_switch.b, switch.out → lens.camera, and the old
    //    orbit→lens wire dropped. The minted switch is applied ENABLED
    //    (select = B).
    let switch = applied
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == "loop_cam_switch")
        .expect("loop_cam_switch minted (D5 Switch enable wiring)");
    assert_eq!(
        switch.params.get("select"),
        Some(&SerializedParamValue::Enum { value: 1 }),
        "applied enabled: select = B (the loop camera)"
    );
    assert!(
        applied
            .wires
            .iter()
            .any(|w| w.from_node == loop_camera.id && w.to_node == switch.id && w.to_port == "b"),
        "loop_camera must feed the switch's b input"
    );
    assert!(
        applied
            .wires
            .iter()
            .any(|w| w.from_node == switch.id && w.to_port == "camera"),
        "switch.out must feed the lens/render camera port"
    );
    let camera_target = applied
        .wires
        .iter()
        .find(|w| w.from_node == switch.id && w.to_port == "camera")
        .map(|w| w.to_node)
        .expect("switch camera wire");
    assert!(
        !applied.wires.iter().any(|w| {
            w.to_node == camera_target && w.to_port == "camera" && w.from_node != switch.id
        }),
        "the displaced camera producer's wire must be dropped (no double-feed)"
    );

    // 3. Every object group gained the interface `instances` input + inner
    //    group_input wire + top-level scene_array wire (the flat view is
    //    authoritative — the runtime flattens groups away).
    let flat = manifold_core::flatten::flatten_groups(applied).expect("flat applied");
    let scene_object_ids: Vec<u32> = flat
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.scene_object")
        .map(|n| n.id)
        .collect();
    assert_eq!(
        scene_object_ids.len(),
        report.object_count,
        "every imported object group must have a scene_object"
    );
    for so in &scene_object_ids {
        assert!(
            flat.wires
                .iter()
                .any(|w| w.to_node == *so && w.to_port == "instances"),
            "scene_object {so} must be wired from scene_array through the interface"
        );
    }

    // 4. D7 P4: apply mints exactly the three loop nodes — no fog.
    assert!(
        applied
            .nodes
            .iter()
            .all(|n| n.node_id.as_str() != "loop_fog" && n.node_id.as_str() != "fog_driver"),
        "P4 fog cut: the plan builder must not mint loop_fog or fog_driver"
    );
}
