//! COMPILE_CONTRACT_DESIGN P2 INV3 — data-gated nodes compile pipelines at install.
//!
//! This test constructs each primitive that has a data-driven skip with empty
//! data and asserts that no new PipelineCompile cold touches occur on its first
//! run. Pipelines must be created at install time, not on first use.

use manifold_foundation::cold_touch::{cold_touch_count, ColdTouchKind};

use crate::node_graph::{PrimitiveRegistry, effect_node::EffectNodeContext, Executor, FrameTime, MetalBackend, StateStore, FINAL_OUTPUT_TYPE_ID, SOURCE_TYPE_ID};

/// Snapshot the cold-touch counter before an operation.
fn snapshot_cold_touches() -> u64 {
    cold_touch_count(ColdTouchKind::PipelineCompile)
}

/// Assert that no new pipeline compiles occurred since the snapshot.
fn assert_no_new_compiles(before: u64, node_name: &str) {
    let after = cold_touch_count(ColdTouchKind::PipelineCompile);
    assert_eq!(
        before, after,
        "{node_name}: expected no pipeline compiles, but cold touches changed from {before} to {after}"
    );
}

#[test]
fn render_value_overlay_compiles_at_install() {
    let registry = PrimitiveRegistry::with_builtin();
    let device = manifold_gpu::metal::Device::headless();
    let mut executor = Executor::new(registry.clone(), &device);
    let mut store = StateStore::new();
    let backend = MetalBackend::new(&device);

    // Construct render_value_overlay with empty positions (zero detections).
    let graph = r#"{
        "nodes": [{
            "type": "node.value_overlay",
            "id": "overlay",
            "params": {
                "format": 0,
                "color": [0.85, 0.92, 1.0, 1.0],
                "alpha": 1.0,
                "font_scale": 1.0,
                "label_count": 32,
                "offset_x": 0.0,
                "offset_y": 0.0,
                "anchor": 0
            }
        }],
        "connections": []
    }"#;

    let mut def: serde_json::Value = serde_json::from_str(graph).unwrap();
    let mut node = registry.construct_by_type_id("node.value_overlay").unwrap();

    // Install creates the pipeline (via the primitive!'s install hook).
    let before_compile = snapshot_cold_touches();

    // First run with empty data (no detections) must NOT compile.
    let ctx = &mut EffectNodeContext::new(
        &mut executor,
        &mut store,
        FrameTime::ZERO,
        &device,
        &backend,
    );
    node.run(ctx);

    assert_no_new_compiles(before_compile, "render_value_overlay");
}

#[test]
fn blob_detect_ffi_compiles_at_install() {
    let registry = PrimitiveRegistry::with_builtin();
    let device = manifold_gpu::metal::Device::headless();
    let mut executor = Executor::new(registry.clone(), &device);
    let mut store = StateStore::new();
    let backend = MetalBackend::new(&device);

    let mut node = registry.construct_by_type_id("node.blob_tracker").unwrap();

    // Install creates the pipeline.
    let before_compile = snapshot_cold_touches();

    // First run must NOT compile (even with zero detections).
    let ctx = &mut EffectNodeContext::new(
        &mut executor,
        &mut store,
        FrameTime::ZERO,
        &device,
        &backend,
    );
    node.run(ctx);

    assert_no_new_compiles(before_compile, "blob_detect_ffi");
}

#[test]
fn render_text_compiles_at_install() {
    let registry = PrimitiveRegistry::with_builtin();
    let device = manifold_gpu::metal::Device::headless();
    let mut executor = Executor::new(registry.clone(), &device);
    let mut store = StateStore::new();
    let backend = MetalBackend::new(&device);

    let mut node = registry.construct_by_type_id("node.render_text").unwrap();

    // Install creates the pipeline.
    let before_compile = snapshot_cold_touches();

    // First run with empty text must NOT compile.
    let ctx = &mut EffectNodeContext::new(
        &mut executor,
        &mut store,
        FrameTime::ZERO,
        &device,
        &backend,
    );
    node.run(ctx);

    assert_no_new_compiles(before_compile, "render_text");
}

#[test]
fn spawn_from_mesh_compiles_at_install() {
    let registry = PrimitiveRegistry::with_builtin();
    let device = manifold_gpu::metal::Device::headless();
    let mut executor = Executor::new(registry.clone(), &device);
    let mut store = StateStore::new();
    let backend = MetalBackend::new(&device);

    let mut node = registry.construct_by_type_id("node.spawn_from_mesh").unwrap();

    // Install creates the pipelines.
    let before_compile = snapshot_cold_touches();

    // First run with empty mesh must NOT compile.
    let ctx = &mut EffectNodeContext::new(
        &mut executor,
        &mut store,
        FrameTime::ZERO,
        &device,
        &backend,
    );
    node.run(ctx);

    assert_no_new_compiles(before_compile, "spawn_from_mesh");
}
