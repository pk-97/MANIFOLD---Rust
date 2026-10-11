//! The whitewater step: foam, spray and bubble particles seeded from a liquid
//! frame, with their emitters, potentials, lifecycle and the CPU handoff.
//! Sits on manifold-water-liquid; never depends on a solver, the mesher, the
//! rigid crate or manifold-nodes-water.

pub mod primitives;
pub mod whitewater_handoff;
