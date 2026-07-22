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
mod render_scene_pcss;
mod render_scene_shadow_cache;
mod render_scene_shadows;
mod rt_p1_shadow;
mod rt_w0_gbuffer;
mod scene_object_migration_round_trip;
mod scene_viewport_navigate;
mod scene_viewport_session;
mod smoke;
