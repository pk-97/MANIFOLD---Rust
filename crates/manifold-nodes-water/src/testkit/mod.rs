//! Water-specific graph fixtures and proof helpers.

pub mod conformance;

pub mod face_grid_scenes;

pub mod preset_extents;

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
