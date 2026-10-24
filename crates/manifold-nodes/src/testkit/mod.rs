//! Shared catalog proof fixtures; absent from production builds.

pub mod assets;
pub mod open_mesh;
pub mod scene_modifier;
pub use manifold_nodes_scene::testkit::scene_objects;
#[cfg(any(test, feature = "gpu-proofs"))]
pub mod liquid_conformance_fixtures;
pub mod source_roots;

pub mod rt_dynamic;
