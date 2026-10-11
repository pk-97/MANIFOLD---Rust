//! The GPU MLS-MPM liquid solver (Matter): the domain, particle-to-grid,
//! grid update, grid-to-particle, body reaction and the frame it publishes.
//! Sits on manifold-water-liquid and manifold-water-rigid; never depends on
//! another solver, the whitewater step, the mesher or manifold-nodes-water.

pub mod matter;
pub mod primitives;
