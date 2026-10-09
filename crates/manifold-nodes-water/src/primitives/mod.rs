mod advect_whitewater;
mod age_whitewater;
mod clamp_liquid_to_solids;
mod count_surface_edges;
pub mod count_surface_triangles;
mod crossing_distance;
manifold_core::testkit_visible! { pub(crate) mod dot_products; }
mod dust_potential;
manifold_core::testkit_visible! { pub(crate) mod emission_count; }
manifold_core::testkit_visible! { pub(crate) mod energy_potential; }
mod extend_lattice;
#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub mod face_grid_scenes;
manifold_core::testkit_visible! { pub(crate) mod face_sample_component; }
#[cfg(feature = "gpu-proofs")]
pub(crate) mod fluid_surface;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_atom_tests;
manifold_core::testkit_visible! { pub(crate) mod gpu_flip_bodies; }
pub(crate) mod gpu_flip_clock;
manifold_core::testkit_visible! { pub(crate) mod gpu_flip_domain; }
#[cfg(test)]
mod gpu_flip_extension_tests;
pub mod gpu_flip_lentine;
pub(crate) mod gpu_flip_narrow_band;
#[cfg(test)]
mod gpu_flip_narrow_band_tests;
pub mod gpu_flip_preset;
manifold_core::testkit_visible! { pub(crate) mod gpu_flip_pressure; }
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_pressure_tests;


pub(crate) mod gpu_flip_sheeting;
#[cfg(test)]
mod gpu_flip_sheeting_cpu_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_step_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_tests;
manifold_core::testkit_visible! { pub(crate) mod gpu_flip_step; }
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_step_tests;
#[cfg(all(any(test, feature = "testkit"), feature = "water-race-probes"))]
pub mod gpu_flip_still;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub mod gpu_flip_volume;
mod inside_turbulence_potential;
manifold_core::testkit_visible! { pub(crate) mod jitter_particles; }
mod keep_whitewater;
manifold_core::testkit_visible! { pub(crate) mod lattice_bricks; }
#[cfg(test)]
mod lattice_closing_tests;
mod lattice_curvature;
pub mod liquid_bricks;
mod liquid_cells;
pub(crate) mod liquid_fill;
manifold_core::testkit_visible! { pub(crate) mod liquid_frame; }
mod liquid_solid_distance;
pub(crate) mod liquid_state;
pub mod liquid_stats;
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_surface_tests;
mod matter_body_reaction;
mod matter_common;
pub(crate) mod matter_domain;
manifold_core::testkit_visible! { pub(crate) mod matter_face_component; }
pub(crate) mod matter_fill;
mod matter_frame;
mod matter_grid_update;
mod matter_move_bodies;
mod matter_state;
mod matter_stats;
mod matter_to_grid;
mod nearest_crossing;
pub mod offset_lattice;
mod pad_distance_lattice;
manifold_core::testkit_visible! { pub(crate) mod particle_identity; }
manifold_core::testkit_visible! { pub(crate) mod particle_publication; }
manifold_core::testkit_visible! { pub(crate) mod particle_volume; }
pub mod physics_world;
pub(crate) mod prefix_scan;
mod preserve_foam;
manifold_core::testkit_visible! { pub(crate) mod push_out_of_solid; }
pub mod redistance_lattice;
pub mod relax_surface_mesh;
mod retype_whitewater;
mod running_total;
manifold_core::testkit_visible! { pub(crate) mod sample_faces_at_particles; }
mod shape_particle_blobs;
mod smooth_lattice;
// Keep this declaration outside a macro: the module exports float_param!.
#[cfg(any(test, feature = "testkit"))]
pub mod sort_particles_into_cells;
#[cfg(not(any(test, feature = "testkit")))]
pub(crate) mod sort_particles_into_cells;
manifold_core::testkit_visible! { pub(crate) mod spawn_whitewater; }
mod surface_crossings;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_parity;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
mod turbulence_emission_count;
mod turbulence_field;
pub mod upwind_distance;
manifold_core::testkit_visible! { pub(crate) mod volume_surface_mesh; }
manifold_core::testkit_visible! { pub(crate) mod wavecrest_potential; }
#[cfg(test)]
mod whitewater_cpu;
pub(crate) mod whitewater_distance;
#[cfg(any(test, all(feature = "testkit", feature = "gpu-proofs")))]
mod whitewater_emitter_cpu;
mod whitewater_emitter_dispatch;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_emitter_gpu_tests;
mod whitewater_emitter_velocity;
#[cfg(test)]
mod whitewater_engine_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_engine_gpu_tests;
#[cfg(test)]
mod whitewater_extent_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_field_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_grid_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_handoff_tests;
mod whitewater_influence;
pub(crate) mod whitewater_lifecycle;
mod whitewater_obstacle_source;
#[cfg(any(test, feature = "testkit"))]
mod whitewater_particle_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_particle_tests;
#[cfg(test)]
mod whitewater_pool_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_pool_tests;
manifold_core::testkit_visible! { pub(crate) mod whitewater_step; }
#[cfg(test)]
mod whitewater_step_tests;
manifold_core::testkit_visible! { pub(crate) mod whitewater_type; }
manifold_core::testkit_visible! { pub(crate) mod blob_bounds; }
#[cfg(test)]
mod face_grid_extent_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod face_grid_tests;
manifold_core::testkit_visible! { pub(crate) mod fluid_role_source; }
mod grid_to_matter;
mod rigid_body;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
mod gpu_flip_tile_tests;
mod apply_radial_burst_3d_to_particles;
mod apply_radial_burst_to_particles;
#[cfg(any(test, feature = "testkit"))]
pub mod euler_step_particles;
#[cfg(not(any(test, feature = "testkit")))]
mod euler_step_particles;
mod euler_step_particles_3d;
mod vector_fields;
#[cfg(any(test, feature = "testkit"))]
pub mod smooth_surface_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod smooth_surface_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_normals;
#[cfg(not(any(test, feature = "testkit")))]
mod surface_mesh_normals;
