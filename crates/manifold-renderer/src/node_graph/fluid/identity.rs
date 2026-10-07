//! One source identity for recorded native replay and computed frame caches.
//! Render materials and project appearance are deliberately outside this seam.
use sha2::{Digest, Sha256};

/// Family-owned source inputs that invalidate recorded physics results.
pub struct PhysicsSourceIdentity {
    pub name: &'static str,
    pub identity: &'static str,
}

inventory::collect!(PhysicsSourceIdentity);

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
    static SOURCES: std::sync::LazyLock<Vec<&'static PhysicsSourceIdentity>> = std::sync::LazyLock::new(|| {
        let mut sources: Vec<_> = inventory::iter::<PhysicsSourceIdentity>.into_iter().collect();
        sources.sort_unstable_by_key(|source| source.name);
        assert!(sources.windows(2).all(|pair| pair[0].name != pair[1].name), "duplicate physics source identity");
        sources
    });
    for source in SOURCES.iter() {
        hash.update((source.identity.len() as u64).to_le_bytes());
        hash.update(source.identity.as_bytes());
    }
    hash.finalize().into()
}
