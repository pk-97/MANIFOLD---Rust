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
    static SOURCES: std::sync::LazyLock<Vec<&'static PhysicsSourceIdentity>> = std::sync::LazyLock::new(|| {
        let mut sources: Vec<_> = inventory::iter::<PhysicsSourceIdentity>.into_iter().collect();
        sources.sort_unstable_by_key(|source| source.name);
        assert!(sources.windows(2).all(|pair| pair[0].name != pair[1].name), "duplicate physics source identity");
        sources
    });
    compose_solver_identity(
        [
            manifold_fluids::SOURCE_IDENTITY,
            manifold_physics::SOURCE_IDENTITY,
            manifold_core::SOURCE_IDENTITY,
            env!("MANIFOLD_PHYSICS_INTEGRATION_IDENTITY"),
        ],
        &SOURCES,
    )
}

fn compose_solver_identity(engine_sources: [&str; 4], family_sources: &[&PhysicsSourceIdentity]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"manifold-physics-sources-v1\0");
    for source in engine_sources {
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source.as_bytes());
    }
    for source in family_sources {
        hash.update((source.identity.len() as u64).to_le_bytes());
        hash.update(source.identity.as_bytes());
    }
    hash.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solver_identity_composes_engine_and_family_sources() {
        let sources = ["fixture fluids", "fixture physics", "fixture core", "fixture integration"];
        let family = PhysicsSourceIdentity { name: "fixture family", identity: "fixture family identity" };
        let mut expected = Sha256::new();
        expected.update(b"manifold-physics-sources-v1\0");
        for identity in sources.into_iter().chain([family.identity]) {
            expected.update((identity.len() as u64).to_le_bytes());
            expected.update(identity.as_bytes());
        }
        let expected: [u8; 32] = expected.finalize().into();
        assert_eq!(compose_solver_identity(sources, &[&family]), expected);
        for index in 0..sources.len() {
            let mut changed = sources;
            changed[index] = "changed engine identity";
            assert_ne!(compose_solver_identity(changed, &[&family]), expected);
        }
        let changed_family = PhysicsSourceIdentity { name: family.name, identity: "changed family identity" };
        assert_ne!(compose_solver_identity(sources, &[&changed_family]), expected);
        assert_ne!(compose_solver_identity(sources, &[]), expected);
    }
}
