//! One source identity for recorded native replay and computed frame caches.
//! Render materials and project appearance are deliberately outside this seam.
use sha2::{Digest, Sha256};

pub(in crate::node_graph) fn solver_identity() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"manifold-physics-sources-v1\0");
    for source in [
        manifold_fluids::SOURCE_IDENTITY,
        manifold_physics::SOURCE_IDENTITY,
        env!("MANIFOLD_PHYSICS_INTEGRATION_IDENTITY"),
    ] {
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source.as_bytes());
    }
    hash.finalize().into()
}
