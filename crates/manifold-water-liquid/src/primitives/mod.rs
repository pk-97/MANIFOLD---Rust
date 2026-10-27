//! Seam atoms: every liquid solver dispatches these, so their node types and
//! helpers are the liquid contract.
pub mod dot_products;
pub mod field_smooth_blend;
pub mod fluid_role_source;
pub mod liquid_cells;
pub mod liquid_stats;
pub mod offset_lattice;
pub mod particle_identity;
pub mod particle_publication;
pub mod prefix_scan;
pub mod running_total;
// Keep this declaration outside a macro: the module exports float_param!.
pub mod sort_particles_into_cells;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
pub mod upwind_distance;
pub mod whitewater_distance;
