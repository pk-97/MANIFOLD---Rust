//! Water-specific graph fixtures and proof helpers.

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub mod physics_history;

#[cfg(test)]
pub(crate) mod physics_fixtures;

#[cfg(any(test, feature = "testkit"))]
pub mod particle_volume;

pub mod fluid_role_source;

#[cfg(any(test, feature = "testkit"))]
pub mod liquid_extents;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod liquid_surface;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod whitewater_scene;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod whitewater_fingerprints;
