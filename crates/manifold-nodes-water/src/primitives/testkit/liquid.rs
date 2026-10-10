//! Liquid-seam node constructors for family proofs.
use manifold_node_engine::primitive::Primitive;
use manifold_gpu::{GpuDevice, GpuComputePipeline};
pub fn running_total() -> impl Primitive { crate::primitives::running_total::RunningTotal::new() }
pub fn smooth_lattice(pipeline: Option<GpuComputePipeline>) -> impl Primitive {
    let mut node = crate::primitives::smooth_lattice::SmoothLattice::new();
    node.pipeline = pipeline;
    node
}
pub fn dense_pipeline(kind: &str, device: &GpuDevice, source: &str) -> GpuComputePipeline {
    let source = match kind {
        "smooth_lattice" => manifold_node_engine::testkit::shader_source::dense_source::<crate::primitives::smooth_lattice::SmoothLattice>(source),
        _ => panic!("no dense water fixture for {kind}"),
    };
    device.create_compute_pipeline(&source, "cs_main", "liquid.bricks.dense_reference")
}
