mod advect_whitewater;
mod age_whitewater;
mod count_surface_edges;
pub mod count_surface_triangles;
mod crossing_distance;
mod dust_potential;
manifold_core::testkit_visible! { pub(crate) mod emission_count; }
manifold_core::testkit_visible! { pub(crate) mod energy_potential; }
mod extend_lattice;
mod inside_turbulence_potential;
manifold_core::testkit_visible! { pub(crate) mod jitter_particles; }
mod keep_whitewater;
manifold_core::testkit_visible! { pub(crate) mod lattice_bricks; }
#[cfg(test)]
mod lattice_closing_tests;
mod lattice_curvature;
manifold_core::testkit_visible! { pub(crate) mod liquid_frame; }
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
mod pad_distance_lattice;
manifold_core::testkit_visible! { pub(crate) mod particle_volume; }
mod preserve_foam;
pub mod relax_surface_mesh;
mod retype_whitewater;
manifold_core::testkit_visible! { pub(crate) mod sample_faces_at_particles; }
mod shape_particle_blobs;
manifold_core::testkit_visible! { pub(crate) mod spawn_whitewater; }
mod surface_crossings;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_parity;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
mod turbulence_emission_count;
mod turbulence_field;
manifold_core::testkit_visible! { pub(crate) mod volume_surface_mesh; }
manifold_core::testkit_visible! { pub(crate) mod wavecrest_potential; }
#[cfg(test)]
mod whitewater_cpu;
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
#[cfg(any(test, all(feature = "testkit", feature = "gpu-proofs")))]
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
mod grid_to_matter;

#[cfg(any(test, feature = "testkit"))]
pub mod smooth_surface_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod smooth_surface_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_normals;
#[cfg(not(any(test, feature = "testkit")))]
mod surface_mesh_normals;
#[cfg(test)]
mod liquid_bricks_consumer_tests;
// The counting-sort proofs cover matter records, so they link from here (D8).
#[cfg(all(test, feature = "gpu-proofs"))]
mod sort_particles_into_cells {
    mod gpu_tests;
}
// The pose agreement proof names FLIP and Matter, so it links from here (D8).
#[cfg(test)]
mod liquid_solid_distance {
    mod tests;
}
