mod acceleration;
mod atomic;
mod execution_plan;
mod expand_acceleration;
mod expand_compiler_conformance;
mod expand_compiler_parameter_guard_tests;
mod expand_compiler_tests;
mod expand_frames;
mod expand_impulses;
mod expand_parameter_guards;
pub(crate) mod math_view_fixtures;
mod palette;
mod persistence;
#[cfg(all(test, feature = "gpu-proofs"))]
mod runtime_array_buffers;
#[cfg(test)]
mod runtime_bool_convert_heal;
#[cfg(test)]
mod runtime_bound_param_survives_rebuild;
#[cfg(all(test, feature = "gpu-proofs"))]
mod runtime_chain_fusion;
#[cfg(test)]
mod runtime_generator_runtime;
#[cfg(test)]
mod runtime_math_view;
#[cfg(test)]
mod runtime_modifier_events;
mod snapshot;

#[cfg(feature = "gpu-proofs")]
pub(crate) mod fusion_proof;

#[cfg(feature = "gpu-proofs")]
pub(crate) mod audio_visual;

mod resource_allocation;

mod liquid_lattice;

mod liquid_extent;

mod validate;

mod fusion_report;

mod refusal_census;

mod freeze_install;

mod freeze_region;

mod physics_sampling;

mod gpu_flip_surface;

mod physics_host_automation;
mod physics_host_modulation;

mod wind_gust_replay;

mod expand_shatter;

#[cfg(feature = "gpu-proofs")]
pub(crate) mod whitewater_scene;

#[cfg(feature = "gpu-proofs")]
pub(crate) mod whitewater_emitters;

mod gpu_flip_preset;

#[cfg(feature = "gpu-proofs")]
pub(crate) mod gpu_flip_body;

#[cfg(feature = "gpu-proofs")]
mod liquid_prepare;

mod binding_migration;

mod image_fused;
#[cfg(feature = "gpu-proofs")]
mod nested_cubes_geometry;

mod particle_frame_blend_tests;
#[cfg(feature = "gpu-proofs")]
mod particle_publication_gpu_tests;

mod seed_particles_from_texture;

mod gltf_animation_source;

#[cfg(feature = "gpu-proofs")]
mod copy_positions;
