//! Explicitly registered graph fixtures and GPU test support.

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod gpu;
#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod test_camera_pointwise_fixture;
#[cfg(test)]
pub(crate) mod test_multi_output_atomic_fixture;
#[cfg(test)]
pub(crate) mod test_face_lattice_fixture;
#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod test_card;
#[cfg(test)]
pub(crate) mod graph;
#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod codegen_support;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod proof_support;

#[cfg(all(any(test, feature = "testkit"), feature = "gpu-proofs"))]
pub mod physics_history;

#[cfg(any(test, feature = "testkit"))]
pub mod mesh_revision;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod liquid_surface;

#[cfg(any(test, feature = "testkit"))]
pub mod shader_source;

#[cfg(any(test, feature = "testkit"))]
pub mod particle_volume;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod whitewater_scene;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod whitewater_fingerprints;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod atom;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod water_codegen;

#[cfg(any(test, feature = "testkit"))]
pub mod liquid_extents;

#[cfg(test)]
pub(crate) mod fusion_fixtures;

pub mod substep_nodes;

#[cfg(test)]
pub(crate) mod physics_fixtures;

#[cfg(test)]
pub(crate) mod document_fixtures;

#[cfg(feature = "gpu-proofs")]
pub mod gpu_harness;

pub mod fluid_role_source;
