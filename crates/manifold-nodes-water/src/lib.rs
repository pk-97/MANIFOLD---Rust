//! Water and physics graph adapters.
//! Owns native simulation nodes and their graph runtime extensions.
//! Depends on the engine and native solvers, never other node families or UI.

pub mod coupled_frame;
pub mod graph_install;
pub mod fluid_particles;
pub mod fluid_role;
pub mod liquid;
pub mod matter;
pub mod node;
pub mod physics;
pub mod physics_mesh;
pub mod physics_events;
pub mod physics_metrics;
pub(crate) mod physics_scene;
pub(crate) mod migration;
pub mod presets;
pub mod whitewater;
pub(crate) mod whitewater_handoff;
pub mod primitives;
pub mod runtime;
pub(crate) mod vector_field;
mod wire_values;
#[cfg(test)]
mod live_sim_clock_reference;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
