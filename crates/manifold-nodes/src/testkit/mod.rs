//! Shared catalog proof fixtures; absent from production builds.

pub mod assets;
pub mod scene_modifier;
pub mod source_roots;
#[cfg(any(test, feature = "gpu-proofs"))]
pub mod liquid_conformance_fixtures;

pub mod rt_dynamic;
