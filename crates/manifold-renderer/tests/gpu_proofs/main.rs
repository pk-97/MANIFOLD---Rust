//! GPU-proof integration binary.
//!
//! Slow, GPU-bound integration tests that need a real Metal device and
//! readback. Gated behind the `gpu-proofs` cargo feature so the default
//! `cargo test` / `cargo nextest` sweep stays fast and non-flaky — run
//! deliberately with `cargo test -p manifold-renderer --features gpu-proofs`.
//!
//! Two suites live here, both sharing one `harness::shared()` device so the
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

mod harness;

mod alpha_contract;
mod bug237_light_camera_commit_render_proof;
mod camera_conformance;
mod film_grain_decorrelation;
mod fragment_storage;
mod gbuffer_depth;
mod gbuffer_velocity;
mod render_scene_exposure;
mod render_scene_fog;
mod render_scene_glass;
mod render_scene_ibl;
mod render_scene_instances;
mod render_scene_lights;
mod render_scene_map_set;
mod render_scene_object_visibility;
mod render_scene_ao_mask;
mod render_scene_pcss;
mod render_scene_shadow_cache;
mod render_scene_shadows;
mod rt_object_motion_shadow;
mod rt_p1_region_probe;
mod rt_p1_shadow;
mod rt_p2_soft_ao_temporal;
mod rt_p3_emissive_gi;
mod rt_p3_emissive_texture;
mod rt_p4_metalfx_temporal;
mod rt_t1a_ghost_speckle;
mod rt_t1b_vertex_normals;
mod rt_t2a_alpha_mask;
mod rt_bug17r3_lightless_gi;
mod rt_bug318_import_toggle;
mod rt_bug326_fix_gate;
mod rt_bug88m_blend_specular_gate;
mod rt_edc_enclosure;
mod rt_emissive_direct;
mod rt_emissive_light_table;
mod rt_furnace_oracle;
mod rt_gesture_response;
mod rt_normal_tangent_mirror;
mod rt_multi_caster_shadow;
mod rt_6caster_shadow;
mod rt_object_cast_shadows;
mod rt_r1_reflection;
mod rt_r2_accumulation;
mod rt_r2_clamp;
mod rt_r3_heldout_gltf;
mod rt_r3_textured_roughness;
mod rt_t2b_temporal_wiring;
mod rt_t2c_shadow_temporal_stability;
mod rt_t38_multibounce;
mod rt_tl_b_transmission;
mod rt_tl_c_sun_tint;
mod rt_w0_gbuffer;
mod scene_object_migration_round_trip;
mod scene_viewport_navigate;
mod scene_viewport_session;
mod smoke;
