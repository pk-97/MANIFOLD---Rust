//! GPU FLIP node constructors for family proofs.
use manifold_node_engine::primitive::Primitive;
use manifold_gpu::{GpuDevice, GpuComputePipeline};
pub fn clamp_liquid_to_solids(pipeline: Option<GpuComputePipeline>) -> impl Primitive {
    let mut node = crate::primitives::clamp_liquid_to_solids::ClampLiquidToSolids::new();
    node.pipeline = pipeline;
    node
}
pub fn dense_pipeline(kind: &str, device: &GpuDevice, source: &str) -> GpuComputePipeline {
    let source = match kind {
        "clamp_liquid_to_solids" => manifold_node_engine::testkit::shader_source::dense_source::<crate::primitives::clamp_liquid_to_solids::ClampLiquidToSolids>(source),
        _ => panic!("no dense water fixture for {kind}"),
    };
    device.create_compute_pipeline(&source, "cs_main", "liquid.bricks.dense_reference")
}
