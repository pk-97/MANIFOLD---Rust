//! The whitewater step: foam, spray and bubble particles seeded from a liquid
//! frame, with their emitters, potentials, lifecycle and the CPU handoff.
//! Sits on the liquid seam; never names another solver.

pub mod primitives;
pub mod whitewater_handoff;
