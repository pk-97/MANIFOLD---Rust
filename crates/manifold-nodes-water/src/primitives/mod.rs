mod count_surface_edges;
pub mod count_surface_triangles;
manifold_core::testkit_visible! { pub(crate) mod lattice_bricks; }
#[cfg(test)]
mod lattice_closing_tests;
manifold_core::testkit_visible! { pub(crate) mod liquid_frame; }
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_surface_tests;
manifold_core::testkit_visible! { pub(crate) mod particle_volume; }
pub mod relax_surface_mesh;
mod shape_particle_blobs;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_parity;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
manifold_core::testkit_visible! { pub(crate) mod volume_surface_mesh; }

manifold_core::testkit_visible! { pub(crate) mod blob_bounds; }
#[cfg(test)]
mod face_grid_extent_tests;
// The whitewater extent proof sizes against Matter and the particle volume,
// so it links from here (D8).
#[cfg(test)]
mod whitewater_extent_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod face_grid_tests;

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
