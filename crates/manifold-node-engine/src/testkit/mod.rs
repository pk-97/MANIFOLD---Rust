//! Explicitly registered graph fixtures and GPU test support.

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod gpu;
#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod test_camera_pointwise_fixture;
#[cfg(any(test, feature = "testkit"))]
pub mod test_multi_output_atomic_fixture;
#[cfg(test)]
pub(crate) mod test_face_lattice_fixture;
#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod test_card;
#[cfg(test)]
pub(crate) mod graph;
#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod codegen_support;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod proof_support;

#[cfg(any(test, feature = "testkit"))]
pub mod mesh_revision;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod array_harness;

#[cfg(any(test, feature = "testkit"))]
pub mod shader_source;

#[cfg(feature = "gpu-proofs")]
#[cfg(any(test, feature = "testkit"))]
pub mod atom;

#[cfg(test)]
pub(crate) mod fusion_fixtures;

pub mod substep_nodes;

#[cfg(any(test, feature = "testkit"))]
pub mod document_fixtures;

#[cfg(feature = "gpu-proofs")]
pub mod gpu_harness;
