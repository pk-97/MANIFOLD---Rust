//! Water and physics graph adapters.
//! Owns native simulation nodes and their graph runtime extensions.
//! Depends on the engine and native solvers, never other node families or UI.

use manifold_water_rigid as _;

pub mod graph_install;
pub mod fluid_particles;
pub mod fluid_role;
pub mod liquid;
pub mod matter;
pub(crate) mod physics_scene;
pub(crate) mod migration;
pub mod presets;
pub mod whitewater;
pub(crate) mod whitewater_handoff;
pub mod primitives;
pub mod runtime;
mod wire_values;
#[cfg(test)]
mod live_sim_clock_reference;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;

/// Prewarm the fixed pipelines of every linked water crate.
pub fn prewarm_pipelines(device: &manifold_gpu::GpuDevice) {
    manifold_water_rigid::primitives::physics_world::PhysicsWorldNode::prewarm_pipeline(device);
}
