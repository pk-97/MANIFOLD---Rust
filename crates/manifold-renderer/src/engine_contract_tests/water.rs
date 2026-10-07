#[path = "water_primitives.rs"]
pub(crate) mod primitives;

#[cfg(feature = "gpu-proofs")]
#[path = "water_physics_scene.rs"]
mod physics_scene;
