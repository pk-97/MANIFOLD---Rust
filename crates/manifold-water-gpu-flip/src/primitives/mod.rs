//! The GPU FLIP solver: its step, pressure solve, clock, bodies and the
//! particle atoms only FLIP dispatches.
#[cfg(feature = "gpu-proofs")]
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_atom_tests;
manifold_core::testkit_visible! { pub(crate) mod gpu_flip_bodies; }
manifold_core::testkit_visible! { pub(crate) mod gpu_flip_clock; }
pub mod gpu_flip_domain;
#[cfg(test)]
mod gpu_flip_extension_tests;
pub mod gpu_flip_lentine;
pub mod gpu_flip_pressure;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_pressure_tests;

pub(crate) mod gpu_flip_sheet_distance;
pub(crate) mod gpu_flip_sheeting;
#[cfg(test)]
mod gpu_flip_sheeting_cpu_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_step_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_tests;
pub mod gpu_flip_step;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_step_tests;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub mod gpu_flip_volume;
pub(crate) mod liquid_fill;
manifold_core::testkit_visible! { pub(crate) mod liquid_solid_distance; }
pub(crate) mod liquid_state;
pub use liquid_state::births_held_back;
#[cfg(any(test, feature = "testkit"))]
pub use liquid_state::retire_stats_for_test;
manifold_core::testkit_visible! { pub(crate) mod push_out_of_solid; }
mod apply_radial_burst_3d_to_particles;
mod apply_radial_burst_to_particles;
#[cfg(any(test, feature = "testkit"))]
pub mod euler_step_particles;
#[cfg(not(any(test, feature = "testkit")))]
mod euler_step_particles;
mod euler_step_particles_3d;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
mod gpu_flip_tile_tests;
