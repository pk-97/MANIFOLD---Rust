mod liquid_bricks;
mod whitewater_step;
mod face_grid_extent_tests;
mod fluid_role_source;

#[cfg(feature = "gpu-proofs")]
pub(crate) mod gpu_flip_scene_tests;

#[cfg(feature = "gpu-proofs")]
mod gpu_flip_tile_tests;

#[cfg(feature = "gpu-proofs")]
mod whitewater_golden_tests;

