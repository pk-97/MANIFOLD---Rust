//! The liquid surface mesher: particle frames to a lattice, bricks, a welded
//! triangle mesh, its smoothing and normals, and the blob bounds.
//! Sits on the liquid seam; never names a solver.

pub mod primitives;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit {
    pub mod liquid_surface;
}
