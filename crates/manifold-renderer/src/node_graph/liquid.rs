//! What every GPU liquid domain shares (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! D2): the fixed-tick clock, the body rows and their distance atlas, the
//! rigid owner that couples Box3D one settled tick at a time, the A/B frame
//! ring of the particle-frame seam, and the face grid layout every solver
//! publishes. Solver rules (substep bounds, reaction encodings, lattice
//! padding) stay with each solver.

pub mod bodies;
pub mod clock;
pub mod coupling;
pub mod frame_ring;
pub mod grid;
