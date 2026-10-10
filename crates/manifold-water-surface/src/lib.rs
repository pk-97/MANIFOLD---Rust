//! The liquid surface mesher: particle frames to a lattice, bricks, a welded
//! triangle mesh, its smoothing and normals, and the blob bounds.
//! Sits on manifold-water-liquid; never depends on a solver, the whitewater
//! step, the rigid crate, manifold-physics or manifold-nodes-water.

pub mod primitives;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit {
    pub mod liquid_surface;
}
