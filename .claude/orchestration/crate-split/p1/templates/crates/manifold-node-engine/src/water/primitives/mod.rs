pub(crate) mod fluid_surface;
mod liquid_solid_distance;
mod matter_body_reaction;
mod matter_common;
pub(crate) mod matter_domain;
pub(crate) mod matter_fill;
mod matter_frame;
mod matter_grid_update;
mod matter_move_bodies;
mod matter_state;
mod matter_stats;
mod matter_to_grid;
pub(crate) mod prefix_scan;
pub(crate) mod sort_particles_into_cells;
mod running_total;
pub(crate) mod particle_volume;
pub(crate) mod lattice_bricks;
pub(crate) mod liquid_bricks;
pub(crate) mod dot_products;
pub(crate) mod gpu_flip_bodies;
pub mod gpu_flip_lentine;
pub(crate) mod gpu_flip_pressure;
pub(crate) mod gpu_flip_step;
pub(crate) mod gpu_flip_sheeting;
#[cfg(test)]
mod gpu_flip_sheeting_cpu_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_sheeting_step_tests;
pub(crate) mod gpu_flip_clock;
pub(crate) mod gpu_flip_narrow_band;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_pressure_tests;
pub(crate) mod liquid_fill;
pub(crate) mod liquid_state;
pub mod liquid_stats;
pub(crate) mod liquid_frame;
pub(crate) mod particle_identity;
pub(crate) mod particle_publication;
pub(crate) mod gpu_flip_domain;
pub(crate) mod face_sample_component;
pub(crate) mod matter_face_component;
mod surface_crossings;
mod nearest_crossing;
mod crossing_distance;
mod liquid_cells;
mod lattice_curvature;
mod extend_lattice;
mod energy_potential;
mod whitewater_obstacle_source;
mod whitewater_emitter_dispatch;
mod whitewater_influence;
mod whitewater_emitter_velocity;
#[cfg(test)]
mod whitewater_emitter_cpu;
#[cfg(test)]
mod whitewater_engine_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_emitter_gpu_tests;
mod emission_count;
mod whitewater_type;
mod preserve_foam;
mod keep_whitewater;
#[cfg(test)]
mod whitewater_pool_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_pool_tests;
pub(crate) mod whitewater_lifecycle;
pub(crate) mod whitewater_step;
mod pad_distance_lattice;
#[cfg(test)]
mod whitewater_step_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_handoff_tests;
#[cfg(test)]
mod whitewater_cpu;
#[cfg(test)]
mod whitewater_particle_cpu;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_particle_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_grid_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_field_tests;
#[cfg(test)]
mod whitewater_extent_tests;
pub mod gpu_flip_preset;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_atom_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_body_tests;
#[cfg(test)]
mod gpu_flip_extension_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_step_tests;
#[cfg(test)]
mod gpu_flip_narrow_band_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_scene_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_flip_tile_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_scene_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_golden_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_prepare_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod gpu_flip_volume;
#[cfg(all(test, feature = "water-race-probes"))]
pub(crate) mod gpu_flip_race_tests;
#[cfg(all(test, feature = "water-race-probes"))]
pub(crate) mod gpu_flip_still;
#[cfg(all(test, feature = "water-race-probes"))]
mod gpu_flip_render_smoke_tests;
mod count_surface_triangles;
pub(crate) mod volume_surface_mesh;
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_surface_tests;
pub(crate) mod physics_world;
pub(crate) mod push_out_of_solid;
#[cfg(test)]
mod lattice_closing_tests;
pub mod upwind_distance;
pub(crate) mod whitewater_distance;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_engine_gpu_tests;
pub use liquid_solid_distance::LiquidSolidDistance;
pub use matter_body_reaction::MatterBodyReaction;
pub use matter_domain::MatterDomain;
pub use matter_fill::MatterFill;
pub use matter_frame::MatterFrame;
pub use matter_grid_update::MatterGridUpdate;
pub use matter_move_bodies::MatterMoveBodies;
pub use matter_state::MATTER_STATE_PORTS;
pub use matter_state::MatterState;
pub use matter_stats::MatterStats;
pub use matter_to_grid::MatterToGrid;
pub use push_out_of_solid::PushOutOfSolid;

pub mod advect_whitewater;

pub mod age_whitewater;

pub mod clamp_liquid_to_solids;

pub mod count_surface_edges;

pub mod dust_potential;

pub mod face_grid_scenes;

pub mod inside_turbulence_potential;

pub mod jitter_particles;

pub mod offset_lattice;

pub mod redistance_lattice;

pub mod relax_surface_mesh;

pub mod retype_whitewater;

pub mod sample_faces_at_particles;

pub mod shape_particle_blobs;

pub mod smooth_lattice;

pub mod spawn_whitewater;

pub mod surface_mesh_parity;

pub mod turbulence_emission_count;

pub mod turbulence_field;

pub mod wavecrest_potential;
