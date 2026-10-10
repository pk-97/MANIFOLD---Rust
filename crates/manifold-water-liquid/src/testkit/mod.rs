//! Liquid-seam fixtures shared with the solver crates above.

/// The rigid body's cube edge per unit of transform scale.
pub const CUBE_EDGE_PER_SCALE: f32 = 1.154_700_5;

#[cfg(any(test, feature = "testkit"))]
pub mod particle_volume;

pub mod fluid_role_source;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod codegen;
