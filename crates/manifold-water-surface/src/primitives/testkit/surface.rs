//! Surface-mesher node constructors for family proofs.
use manifold_node_engine::primitive::Primitive;
use manifold_gpu::{GpuDevice, GpuComputePipeline};
pub fn shape_particle_blobs() -> impl Primitive { crate::primitives::shape_particle_blobs::ShapeParticleBlobs::new() }
pub fn count_surface_triangles(pipeline: Option<GpuComputePipeline>) -> impl Primitive {
    let mut node = crate::primitives::count_surface_triangles::CountSurfaceTriangles::new();
    node.pipeline = pipeline;
    node
}
pub fn relax_surface_mesh() -> impl Primitive { crate::primitives::relax_surface_mesh::RelaxSurfaceMesh::new() }
pub fn dense_pipeline(kind: &str, device: &GpuDevice, source: &str) -> GpuComputePipeline {
    let source = match kind {
        "count_surface_triangles" => manifold_node_engine::testkit::shader_source::dense_source::<crate::primitives::count_surface_triangles::CountSurfaceTriangles>(source),
        "relax_surface_mesh" => manifold_node_engine::testkit::shader_source::dense_source::<crate::primitives::relax_surface_mesh::RelaxSurfaceMesh>(source),
        _ => panic!("no dense water fixture for {kind}"),
    };
    device.create_compute_pipeline(&source, "cs_main", "liquid.bricks.dense_reference")
}
