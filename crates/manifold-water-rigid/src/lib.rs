//! Box3D rigid bodies as graph nodes: the physics world, rigid body and vector
//! field nodes, the coupled frame, metrics and events, and the native pair
//! contract (`node::PhysicsNode`) a coupled liquid implements. The bottom of
//! the water stack: it never depends on manifold-fluids or any other water
//! crate, and its code names no liquid solver.

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
