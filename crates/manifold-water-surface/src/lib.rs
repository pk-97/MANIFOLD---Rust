//! The liquid surface field: particle frames to a lattice, bricks, the blob
//! bounds and the solid carry.
//! Sits on manifold-water-liquid; never depends on a solver, the whitewater
//! step, the rigid crate, manifold-physics or manifold-nodes-water.

pub mod primitives;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit {
    pub mod liquid_surface;
}
