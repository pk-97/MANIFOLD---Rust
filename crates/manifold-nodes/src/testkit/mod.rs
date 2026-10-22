//! Shared catalog proof fixtures; absent from production builds.

pub mod assets;
pub mod open_mesh;
pub mod scene_modifier;
pub mod scene_objects;
pub mod source_roots;
#[cfg(any(test, feature = "gpu-proofs"))]
pub mod liquid_conformance_fixtures;

pub mod rt_dynamic;
