//! What every GPU liquid domain shares (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! D2): the fixed-tick clock, the padded lattice with its walls, the body
//! rows and their distance atlas, the rigid owner that couples Box3D one
//! settled tick at a time, the A/B frame ring of the particle-frame seam, and
//! the face grid layout every solver publishes. Solver rules (substep bounds,
//! reaction encodings, block sorting) stay with each solver.

pub mod bodies;
pub mod body_buffers;
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
pub mod grid;
pub mod lattice;
#[cfg(test)]
mod scene_contract;

use crate::node_graph::fluid_role::MAX_FLUID_ROLES;

/// The largest count a scalar wire carries exactly: wires are f32, and past
/// 2^24 a count can round up past the storage sized from the true count.
pub const EXACT_F32_COUNT: u32 = 1 << 24;

/// Rest density of water, kg/m³: every liquid solver's water weighs this.
pub const WATER_DENSITY: f32 = 1000.0;

/// A liquid domain's role inputs, in slot order (node.fluid_surface's names).
pub const ROLE_PORTS: [&str; MAX_FLUID_ROLES] = [
    "role_0", "role_1", "role_2", "role_3", "role_4", "role_5", "role_6", "role_7", "role_8",
    "role_9", "role_10", "role_11", "role_12", "role_13", "role_14", "role_15", "role_16", "role_17",
    "role_18", "role_19", "role_20", "role_21", "role_22", "role_23", "role_24", "role_25", "role_26",
    "role_27", "role_28", "role_29", "role_30", "role_31", "role_32", "role_33", "role_34", "role_35",
    "role_36", "role_37", "role_38", "role_39", "role_40", "role_41", "role_42", "role_43", "role_44",
    "role_45", "role_46", "role_47", "role_48", "role_49", "role_50", "role_51", "role_52", "role_53",
    "role_54", "role_55", "role_56", "role_57", "role_58", "role_59", "role_60", "role_61", "role_62",
    "role_63",
];
