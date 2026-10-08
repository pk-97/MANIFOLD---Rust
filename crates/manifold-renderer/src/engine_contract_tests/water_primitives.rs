#[path = "liquid_bricks.rs"]
mod liquid_bricks;
#[path = "whitewater_step.rs"]
mod whitewater_step;

#[cfg(feature = "gpu-proofs")]
#[path = "gpu_flip_scene_tests.rs"]
pub(crate) mod gpu_flip_scene_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "gpu_flip_tile_tests.rs"]
mod gpu_flip_tile_tests;

#[cfg(feature = "gpu-proofs")]
#[path = "whitewater_golden_tests.rs"]
mod whitewater_golden_tests;

#[cfg(feature = "water-race-probes")]
#[path = "gpu_flip_race_tests.rs"]
mod gpu_flip_race_tests;
