//! Physics graph nodes over the shared engine vocabulary: the GPU side of the
//! force lattice that water, matter and swarms sample, and the swarm nodes.
//! Depends on manifold-physics, manifold-gpu and manifold-node-engine only;
//! it names no liquid solver.

pub mod force_lattice;
pub mod primitives;
