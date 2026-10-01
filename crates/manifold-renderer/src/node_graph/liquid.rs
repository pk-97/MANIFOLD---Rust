//! What every GPU liquid domain shares (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! D2): the fixed-tick clock, the padded lattice with its walls, the body
//! rows and their distance atlas, the rigid owner that couples Box3D one
//! settled tick at a time, and the A/B frame ring of the particle-frame
//! seam. Solver rules (substep bounds, reaction encodings, block sorting)
//! stay with each solver.

pub mod bodies;
pub mod clock;
#[cfg(any(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod conformance;
pub mod coupling;
#[cfg(any(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod extent;
pub mod fields;
pub mod frame_ring;
pub mod lattice;
#[cfg(test)]
mod scene_contract;

/// The largest count a scalar wire carries exactly: wires are f32, and past
/// 2^24 a count can round up past the storage sized from the true count.
pub const EXACT_F32_COUNT: u32 = 1 << 24;
