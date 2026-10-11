manifold_core::testkit_visible! { pub(crate) mod blob_bounds; }
manifold_core::testkit_visible! { pub(crate) mod count_surface_edges; }
pub mod count_surface_triangles;
manifold_core::testkit_visible! { pub(crate) mod lattice_bricks; }
#[cfg(test)]
mod lattice_closing_tests;
pub mod liquid_frame;
manifold_core::testkit_visible! { pub(crate) mod particle_volume; }
pub mod relax_surface_mesh;
mod shape_particle_blobs;
#[cfg(any(test, feature = "testkit"))]
pub mod smooth_surface_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod smooth_surface_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_normals;
#[cfg(not(any(test, feature = "testkit")))]
mod surface_mesh_normals;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_parity;
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit;
manifold_core::testkit_visible! { pub(crate) mod volume_surface_mesh; }
