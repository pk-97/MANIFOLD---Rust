//! Seam atoms: every liquid solver dispatches these, so their node types and
//! helpers are the liquid contract (WATER_CRATES_DESIGN.md D3).
pub mod dot_products;
pub mod face_sample_component;
pub mod fluid_role_source;
pub mod liquid_bricks;
pub mod liquid_cells;
pub mod liquid_stats;
pub mod offset_lattice;
pub mod particle_identity;
pub mod particle_publication;
pub mod prefix_scan;
pub mod redistance_lattice;
pub mod running_total;
pub mod smooth_lattice;
// Keep this declaration outside a macro: the module exports float_param!.
pub mod sort_particles_into_cells;
pub mod upwind_distance;
pub mod whitewater_distance;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
