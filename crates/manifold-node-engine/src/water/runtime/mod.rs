pub(crate) mod gpu_flip_surface;
mod physics_carry;
manifold_core::testkit_visible! { pub(crate) mod physics_impulses; }
manifold_core::testkit_visible! { pub(crate) mod physics_sampling; }
#[cfg(feature = "gpu-proofs")]
mod physics_source_chain;
#[cfg(feature = "gpu-proofs")]
mod physics_source_controls;
mod physics_source_runtime;
#[cfg(feature = "gpu-proofs")]
pub(crate) mod physics_source_state;
#[cfg(test)]
#[cfg(feature = "gpu-proofs")]
mod physics_source_state_tests;
#[cfg(feature = "gpu-proofs")]
pub(crate) mod physics_sources;
pub mod scene_impulses;
mod state;
mod access;
mod inspection;
pub use access::{WaterRuntime, WaterRuntimeRef, WaterRuntimeExt};
pub(crate) use state::WaterRuntimeState;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
