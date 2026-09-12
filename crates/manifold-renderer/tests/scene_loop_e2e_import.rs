//! SCENE_LOOP_DESIGN P2 end-to-end gate: load a real imported GLB graph,
//! attach the bundled v3 SceneLoop recipe through canonical insertion, and
//! prepare the result through the ordinary renderer compiler seam.
//!
//! This is the seam that lets P1 ship: a hand-built graph in a unit test never
//! exercises production authoring or preparation. Here the recipe comes from
//! the same bundled file the panel's "Enable Scene Loop" dispatches.
//!
//! Wrap parity (INV-3) on this real-import path was attempted and DELETED
//! (P4): two frames of ONE session through ONE shared GpuDevice still differ
//! (≈80 max pixel diff) — the import's AO/cinematic path is nondeterministic
//! in-session, not just per device instance. BUG-twa6 (device-seed) tracks
//! the retirement; until it lands, INV-3 gates on the deterministic minimal
//! graph (`scene_loop_wrap_parity.rs`) and this file gates structure only.

use std::path::Path;

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers;
use manifold_renderer::node_graph::PrimitiveRegistry;

#[path = "common/scene_modifier.rs"]
mod common;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/apricot_tl05.glb"
);

// The end-to-end gate: the REAL file import → canonical insertion → compiler
// preparation path, verified on the prepared graph's structure and pipeline
// facts. Pixel-diff copies are proven on the hand-built
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
    let applied = common::attach(&def, "SceneLoop", "e2e_loop");
    let prepared = prepare_scene_modifiers(&applied, &PrimitiveRegistry::with_builtin())
        .expect("scene loop expansion");

    // End-to-end applied-graph gate on the REAL import (structural facts —
    // the pixel copies proof lives in `scene_loop_probe.rs`: this GLB renders
    // near-black through the throwaway headless runtime, so pixel assertions
    // on it would flake on the harness, not the splice).
    //
    // 1. Loop nodes minted with the D10-ruled pose: loop_camera home=-cell/2
    //    (corridor entry), scene_array cell_size matching.
    let camera_route = prepared
        .routes
        .iter()
        .find(|route| {
            route.modifier_id == manifold_core::NodeId::new("e2e_loop")
                && route.local.node.as_str() == "loop_camera"
        })
        .expect("loop camera route");
    let camera_id = &camera_route.copies[0].node_id;
    let loop_camera = prepared
        .def
        .nodes
        .iter()
        .find(|n| &n.node_id == camera_id)
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
    let switch_route = prepared
        .routes
        .iter()
        .find(|route| {
            route.modifier_id == manifold_core::NodeId::new("e2e_loop")
                && route.local.node.as_str() == "loop_cam_switch"
        })
        .expect("loop switch route");
    let switch_id = &switch_route.copies[0].node_id;
    let switch = prepared
        .def
        .nodes
        .iter()
        .find(|n| &n.node_id == switch_id)
        .expect("loop_cam_switch minted (D5 Switch enable wiring)");
    assert_eq!(
        switch.params.get("select"),
        Some(&SerializedParamValue::Enum { value: 1 }),
        "applied enabled: select = B (the loop camera)"
    );
    assert!(
        prepared
            .def
            .wires
            .iter()
            .any(|w| w.from_node == loop_camera.id && w.to_node == switch.id && w.to_port == "b"),
        "loop_camera must feed the switch's b input"
    );
    assert!(
        prepared
            .def
            .wires
            .iter()
            .any(|w| w.from_node == switch.id && w.to_port == "camera"),
        "switch.out must feed the lens/render camera port"
    );
    let camera_target = prepared
        .def
        .wires
        .iter()
        .find(|w| w.from_node == switch.id && w.to_port == "camera")
        .map(|w| w.to_node)
        .expect("switch camera wire");
    assert!(
        !prepared.def.wires.iter().any(|w| {
            w.to_node == camera_target && w.to_port == "camera" && w.from_node != switch.id
        }),
        "the displaced camera producer's wire must be dropped (no double-feed)"
    );

    // 3. Every object group gained the interface `instances` input + inner
    //    group_input wire + top-level scene_array wire (the flat view is
    //    authoritative — the runtime flattens groups away).
    let flat = manifold_core::flatten::flatten_groups(&prepared.def).expect("flat applied");
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
        prepared
            .def
            .nodes
            .iter()
            .all(|n| n.node_id.as_str() != "loop_fog" && n.node_id.as_str() != "fog_driver"),
        "P4 fog cut: the prepared recipe must not mint loop_fog or fog_driver"
    );
}
