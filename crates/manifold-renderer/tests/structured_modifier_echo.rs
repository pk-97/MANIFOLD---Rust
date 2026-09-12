//! Schema and standalone-codegen checks for the bounded analytic scene echo.
//!
//! Numerical GPU proofs live beside the primitive so they can share the
//! renderer's `test_device` lock and the crate's GPU-only test configuration.

use manifold_renderer::node_graph::freeze::codegen::standalone_for_spec;
use manifold_renderer::node_graph::primitives::AnalyticEchoInstances;

#[test]
fn structured_modifier_echo_schema_and_standalone_index_contract() {
    let wgsl =
        standalone_for_spec::<AnalyticEchoInstances>().expect("analytic echo standalone codegen");
    assert!(wgsl.contains("let source_idx = idx / 8u"));
    assert!(wgsl.contains("let echo_idx = idx % 8u"));
    assert!(wgsl.contains("arrayLength(&buf_instances)"));
    assert!(wgsl.contains("src.pos_scale.w * taper_factor"));
    assert!(wgsl.contains("scene_radius"));
}
