//! GPU-proof integration binary.
//!
//! Slow, GPU-bound integration tests that need a real Metal device and
//! readback. Gated behind the `gpu-proofs` cargo feature so the default
//! `cargo test` / `cargo nextest` sweep stays fast and non-flaky — run
//! deliberately with `cargo test -p manifold-nodes --features gpu-proofs`.
//!
//! Two suites live here, both sharing one `manifold_node_engine::testkit::gpu_harness::shared()` device so the
//! ~5s `GpuDevice::new()` cost is paid once:
//!
//! - `alpha_contract` — the premultiplied-alpha invariant guard: every
//!   texture→texture effect fed a transparent input must stay transparent.
//! - `smoke` — every bundled generator preset renders one frame with no
//!   NaN/Inf output.
//!
//! (The old per-effect *parity* suite — byte-exact graph-vs-legacy-shader
//! comparisons — was migration scaffolding and was deleted once the legacy
//! effect impls were gone. Nothing runs through a legacy path anymore, so
//! there is nothing left to be "at parity" with.)

use manifold_nodes as _;
mod scene_modifier_legacy;
mod glb_conformance;

mod alpha_contract;
mod node_error_status;
mod bug237_light_camera_commit_render_proof;
mod cinematic_scene_tail;
mod film_grain_decorrelation;
mod fragment_storage;
mod gbuffer_depth;
mod gbuffer_velocity;
mod motion_blur_visibility;
mod render_scene_pbr_fidelity;
mod render_scene_subsurface;
#[path = "catalog/render_scene_material_upgrade.rs"]
mod render_scene_material_upgrade;
mod render_legacy_parity;
mod render_scene_map_set;
mod physics_boxes;
mod physics_takes;
mod fluid_array_growth;
mod render_scene_ao_mask;
mod render_scene_shadow_cache;
mod rt_object_motion_shadow;
mod rt_p2_soft_ao_temporal;
mod rt_p3_emissive_texture;
mod rt_t1b_vertex_normals;
#[path = "catalog/rt_bug318_import_toggle.rs"]
mod rt_bug318_import_toggle;
#[path = "catalog/rt_bug326_fix_gate.rs"]
mod rt_bug326_fix_gate;
#[path = "catalog/rt_bugmajv_kernel_toggle.rs"]
mod rt_bugmajv_kernel_toggle;
mod rt_emissive_instancing;
mod rt_emissive_light_table;
mod rt_firefly_clamp;
mod rt_atrous_post;
mod rt_gesture_response;
mod rt_dynamic_geometry;
mod rt_dynamic_current_frame;
mod rt_dynamic_refit;
#[cfg(feature = "fluid-perf-proofs")]
mod fluid_surface_perf;
#[cfg(feature = "water-race-probes")]
mod gpu_flip_frame_perf;
#[cfg(feature = "matter-perf-proofs")]
mod matter_solver_perf;
mod rt_dynamic_shading;
#[path = "catalog/rt_normal_tangent_mirror.rs"]
mod rt_normal_tangent_mirror;
mod rt_r2_accumulation;
mod rt_r2_clamp;
#[path = "catalog/rt_r3_heldout_gltf.rs"]
mod rt_r3_heldout_gltf;
mod rt_t38_multibounce;
mod rt_w0_gbuffer;
mod scene_object_migration_round_trip;
mod matter_cost_probe;
mod matter_look;
mod matter_scene;
mod matter_transfer;
mod matter_bodies;
mod matter_coupling;
mod liquid_indexed;
mod smoke;
mod substeps;
mod encode_replay;
