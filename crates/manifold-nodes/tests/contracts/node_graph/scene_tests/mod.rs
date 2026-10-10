#[cfg(feature = "gpu-proofs")]
mod fragment_mask_continuity;
mod object_ports;
mod material_validation;

mod mesh_fusion;

mod mesh_revision;

#[cfg(feature = "gpu-proofs")]
mod codegen;
