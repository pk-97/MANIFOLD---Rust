//! The gas solver for Smoke and Fire: a dense box of cells on the World's
//! grid, stepped once a frame on the GPU (see docs/SMOKE_AND_FIRE_MAP.md).
//! Sits on manifold-node-engine; never depends on a liquid solver.

pub mod primitives;
