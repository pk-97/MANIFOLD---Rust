//! Box3D rigid-body graph adapter and the native pair contract that coupled
//! simulations implement. Never names a liquid solver; the liquids depend on it.

pub mod coupled_frame;
pub mod node;
pub mod physics;
pub mod physics_events;
pub mod physics_mesh;
pub mod physics_metrics;
pub mod primitives;
pub(crate) mod vector_field;
mod wire_values;

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
