//! The GPU MLS-MPM liquid solver (Matter): the domain, particle-to-grid,
//! grid update, grid-to-particle, body reaction and the frame it publishes.
//! Sits on the liquid seam; never names another solver.

pub mod matter;
pub mod primitives;
