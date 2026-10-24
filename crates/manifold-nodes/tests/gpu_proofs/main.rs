//! GPU-proof integration binary.
//!
//! Slow, GPU-bound integration tests that need a real Metal device and
//! readback. Gated behind the `gpu-proofs` cargo feature so the default
//! `cargo test` / `cargo nextest` sweep stays fast and non-flaky — run
//! deliberately with `cargo test -p manifold-nodes --features gpu-proofs`.
//!
//! Every module shares one `manifold_node_engine::testkit::gpu_harness::shared()` device, so the
//! ~5s `GpuDevice::new()` cost is paid once. The finite-first-frame sweep over every bundled generator is
//! `bundled_generator_presets::tests::every_bundled_preset_executes_one_frame` in the lib.

use manifold_nodes as _;
mod glb_conformance;
mod scene_pick;
mod scene_viewport_session;

mod alpha_contract;
mod bug237_light_camera_commit_render_proof;
mod cinematic_scene_tail;
mod copy_blend_layout;
mod encode_replay;
mod film_grain_decorrelation;
mod fragment_storage;
mod gbuffer_depth;
mod gbuffer_velocity;
mod liquid_indexed;
mod matter_bodies;
mod matter_coupling;
mod matter_look;
mod matter_scene;
mod matter_transfer;
mod motion_blur_visibility;
mod node_error_status;
mod render_legacy_parity;
mod render_scene_ao_mask;
mod render_scene_map_set;
mod render_scene_pbr_fidelity;
mod render_scene_shadow_cache;
mod render_scene_subsurface;
mod rt_atrous_post;
#[path = "catalog/rt_bug318_import_toggle.rs"]
mod rt_bug318_import_toggle;
#[path = "catalog/rt_bug326_fix_gate.rs"]
mod rt_bug326_fix_gate;
#[path = "catalog/rt_bugmajv_kernel_toggle.rs"]
mod rt_bugmajv_kernel_toggle;
mod rt_dynamic_current_frame;
mod rt_dynamic_geometry;
mod rt_dynamic_refit;
mod rt_dynamic_shading;
mod rt_emissive_instancing;
mod rt_emissive_light_table;
mod rt_firefly_clamp;
mod rt_gesture_response;
#[path = "catalog/rt_normal_tangent_mirror.rs"]
mod rt_normal_tangent_mirror;
mod rt_object_motion_shadow;
mod rt_p2_soft_ao_temporal;
mod rt_p3_emissive_texture;
mod rt_r2_accumulation;
mod rt_r2_clamp;
#[path = "catalog/rt_r3_heldout_gltf.rs"]
mod rt_r3_heldout_gltf;
mod rt_t1b_vertex_normals;
mod rt_t38_multibounce;
mod rt_w0_gbuffer;
mod scene_loop_copies;
mod scene_swarm;
mod substeps;
