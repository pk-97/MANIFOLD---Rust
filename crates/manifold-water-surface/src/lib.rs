//! The liquid surface field: particle frames to the display's own world
//! tiles, the level set and velocity on them, and the solid carry.
//! Sits on manifold-water-liquid; never depends on a solver, the whitewater
//! step, the rigid crate, manifold-physics or manifold-nodes-water.

pub mod primitives;
