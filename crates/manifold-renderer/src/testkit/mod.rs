//! Explicitly registered graph fixtures and GPU test support.

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod gpu;
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
#[cfg(test)]
pub(crate) mod codegen_support;

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod proof_support;

#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod physics_history;

#[cfg(test)]
pub(crate) mod mesh_revision;

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod liquid_surface;

#[cfg(test)]
pub(crate) mod shader_source;

#[cfg(test)]
pub(crate) mod particle_volume;

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod whitewater_scene;

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod whitewater_fingerprints;

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod atom;

#[cfg(feature = "gpu-proofs")]
#[cfg(test)]
pub(crate) mod water_codegen;

#[cfg(test)]
pub(crate) mod liquid_extents;

pub mod substep_nodes;
