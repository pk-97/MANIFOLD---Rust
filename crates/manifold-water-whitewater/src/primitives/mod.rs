mod advect_whitewater;
mod age_whitewater;
manifold_core::testkit_visible! { pub(crate) mod crossing_distance; }
mod dust_potential;
manifold_core::testkit_visible! { pub(crate) mod emission_count; }
manifold_core::testkit_visible! { pub(crate) mod energy_potential; }
manifold_core::testkit_visible! { pub(crate) mod extend_lattice; }
mod inside_turbulence_potential;
manifold_core::testkit_visible! { pub(crate) mod jitter_particles; }
mod keep_whitewater;
manifold_core::testkit_visible! { pub(crate) mod lattice_curvature; }
manifold_core::testkit_visible! { pub(crate) mod nearest_crossing; }
mod pad_distance_lattice;
mod preserve_foam;
mod retype_whitewater;
manifold_core::testkit_visible! { pub(crate) mod sample_faces_at_particles; }
manifold_core::testkit_visible! { pub(crate) mod spawn_whitewater; }
manifold_core::testkit_visible! { pub(crate) mod surface_crossings; }
mod turbulence_emission_count;
mod turbulence_field;
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
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_field_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_grid_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod whitewater_handoff_tests;
mod whitewater_influence;
manifold_core::testkit_visible! { pub(crate) mod whitewater_lifecycle; }
mod whitewater_obstacle_source;
#[cfg(any(test, feature = "testkit"))]
pub mod whitewater_particle_cpu;
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
/// Concrete whitewater-node constructors for family proofs.
#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod testkit {
    pub mod whitewater;
}
