#[path = "water_primitives.rs"]
mod primitives;

#[cfg(feature = "gpu-proofs")]
#[path = "water_physics_scene.rs"]
mod physics_scene;
