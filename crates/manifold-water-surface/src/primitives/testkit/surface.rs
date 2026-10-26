//! Surface node constructors for family proofs.
pub fn shape_particle_blobs() -> impl manifold_node_engine::primitive::Primitive {
    crate::primitives::shape_particle_blobs::ShapeParticleBlobs::new()
}
