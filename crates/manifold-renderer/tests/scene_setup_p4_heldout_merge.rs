// Held-out gate for SCENE_SETUP_PANEL_DESIGN.md P4 — orchestrator-run, not part of the
// executing worker's own test suite (the worker's worktree lacks these gitignored fixtures).
// Merges skull_salazar_downloadable.glb INTO abandoned_warehouse_-_interior_scene.glb's
// imported scene, using the real production parse path end to end.

use manifold_renderer::node_graph::gltf_import::{assemble_import_graph, assemble_merge_plan};
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf")
        .join(name)
}

#[test]
fn merges_skull_into_warehouse_held_out_real_assets() {
    let warehouse = fixture("abandoned_warehouse_-_interior_scene.glb");
    let skull = fixture("skull_salazar_downloadable.glb");
    assert!(warehouse.exists(), "held-out fixture missing: {warehouse:?}");
    assert!(skull.exists(), "held-out fixture missing: {skull:?}");

    let (target_def, target_report) =
        assemble_import_graph(&warehouse).expect("warehouse import must succeed");
    println!("warehouse import report: {target_report:?}");

    let plan = assemble_merge_plan(&target_def, &skull).expect("merge plan must succeed");

    println!(
        "merge plan: {} new nodes, {} new wires, new_objects_count={}, report_lines={:?}",
        plan.new_nodes.len(),
        plan.new_wires.len(),
        plan.new_objects_count,
        plan.report_lines
    );

    assert!(!plan.new_nodes.is_empty(), "skull must contribute object nodes");
    assert!(
        plan.new_objects_count > target_report.object_count as u32,
        "objects count must grow: target had {}, plan says {}",
        target_report.object_count,
        plan.new_objects_count
    );

    // Chrome exclusion: no camera/envmap/light/lens type ids among new nodes.
    let forbidden_type_ids = [
        "node.orbit_camera",
        "node.free_camera",
        "node.look_at_camera",
        "node.bake_environment",
        "node.hdri_source",
        "node.light",
        "node.exposure",
        "node.switch_texture",
    ];
    for node in &plan.new_nodes {
        assert!(
            !forbidden_type_ids.contains(&node.type_id.as_str()),
            "merge plan must never carry chrome, found {}",
            node.type_id
        );
    }

    // Apply the plan to a def (new nodes/wires + bumped `objects` param) and write it out
    // for the orchestrator to run `graph_tool validate`/`fusion` against as a separate step.
    use manifold_core::effect_graph_def::SerializedParamValue;
    let mut merged_def = target_def.clone();
    merged_def.nodes.extend(plan.new_nodes.clone());
    merged_def.wires.extend(plan.new_wires.clone());
    if let Some(render_scene_node) = merged_def
        .nodes
        .iter_mut()
        .find(|n| n.id == plan.render_scene_node_id)
    {
        render_scene_node.params.insert(
            "objects".to_string(),
            SerializedParamValue::Int {
                value: plan.new_objects_count as i32,
            },
        );
    }

    let json = serde_json::to_string_pretty(&merged_def).expect("serialize merged def");
    let out_path = std::env::temp_dir().join("scene_setup_p4_heldout_warehouse_skull.json");
    std::fs::write(&out_path, &json).expect("write merged def for graph_tool");
    println!("merged held-out def written to {out_path:?}");
}
