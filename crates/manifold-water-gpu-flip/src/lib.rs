//! The GPU FLIP liquid solver: step, pressure solve, clock, bodies, sheeting
//! and the particle atoms only FLIP dispatches. Sits on manifold-water-liquid
//! and manifold-water-rigid; never depends on another solver, the whitewater
//! step, the mesher or manifold-nodes-water.

pub mod primitives;
