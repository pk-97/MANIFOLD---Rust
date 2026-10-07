mod advect_whitewater;
mod age_whitewater;
mod clamp_liquid_to_solids;
mod count_surface_edges;
mod count_surface_triangles;
mod crossing_distance;
pub(crate) mod dot_products;
mod dust_potential;
mod emission_count;
mod energy_potential;
mod extend_lattice;
#[cfg(any(test, feature = "gpu-proofs"))]
pub(crate) mod face_grid_scenes;
pub(crate) mod face_sample_component;
#[cfg(feature = "gpu-proofs")]
pub(crate) mod fluid_surface;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_atom_tests;
pub(crate) mod gpu_flip_bodies;
pub(crate) mod gpu_flip_clock;
pub(crate) mod gpu_flip_domain;
#[cfg(test)]
mod gpu_flip_extension_tests;
pub mod gpu_flip_lentine;
pub(crate) mod gpu_flip_narrow_band;
#[cfg(test)]
mod gpu_flip_narrow_band_tests;
pub mod gpu_flip_preset;
pub(crate) mod gpu_flip_pressure;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_pressure_tests;
#[cfg(all(test, feature = "water-race-probes"))]
pub(crate) mod gpu_flip_race_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_scene_tests;
pub(crate) mod gpu_flip_sheeting;
#[cfg(test)]
mod gpu_flip_sheeting_cpu_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_step_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_tests;
pub(crate) mod gpu_flip_step;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_step_tests;
#[cfg(all(test, feature = "water-race-probes"))]
pub(crate) mod gpu_flip_still;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_tile_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod gpu_flip_volume;
mod inside_turbulence_potential;
mod jitter_particles;
mod keep_whitewater;
pub(crate) mod lattice_bricks;
#[cfg(test)]
mod lattice_closing_tests;
mod lattice_curvature;
pub(crate) mod liquid_bricks;
mod liquid_cells;
pub(crate) mod liquid_fill;
pub(crate) mod liquid_frame;
mod liquid_solid_distance;
pub(crate) mod liquid_state;
pub mod liquid_stats;
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_surface_tests;
mod matter_body_reaction;
mod matter_common;
pub(crate) mod matter_domain;
pub(crate) mod matter_face_component;
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
pub(crate) mod particle_identity;
pub(crate) mod particle_publication;
pub(crate) mod particle_volume;
pub(crate) mod physics_world;
pub(crate) mod prefix_scan;
mod preserve_foam;
pub(crate) mod push_out_of_solid;
pub mod redistance_lattice;
mod relax_surface_mesh;
mod retype_whitewater;
mod running_total;
mod sample_faces_at_particles;
mod shape_particle_blobs;
mod smooth_lattice;
pub(crate) mod sort_particles_into_cells;
mod spawn_whitewater;
mod surface_crossings;
#[cfg(test)]
mod surface_mesh_parity;
#[cfg(all(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub(crate) mod testkit;
mod turbulence_emission_count;
mod turbulence_field;
pub mod upwind_distance;
pub(crate) mod volume_surface_mesh;
mod wavecrest_potential;
#[cfg(test)]
mod whitewater_cpu;
pub(crate) mod whitewater_distance;
#[cfg(test)]
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
mod whitewater_golden_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_grid_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_handoff_tests;
mod whitewater_influence;
pub(crate) mod whitewater_lifecycle;
mod whitewater_obstacle_source;
#[cfg(test)]
mod whitewater_particle_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_particle_tests;
#[cfg(test)]
mod whitewater_pool_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_pool_tests;
pub(crate) mod whitewater_step;
#[cfg(test)]
mod whitewater_step_tests;
mod whitewater_type;
