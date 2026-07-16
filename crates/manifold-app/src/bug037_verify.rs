//! BUG-037 (glp-first-render-stall) regression guard — headless,
//! `MANIFOLD_RENDER_TRACE`-driven, same pattern as `bug035_verify.rs`.
//!
//! The backlog entry's diagnosis: first render of a glTF scene layer stalled
//! the content thread ~37ms (`RENDER_TRACE` showed `generators=37.1ms` on the
//! layer's first rendered frame). Root cause: `node.render_scene`'s
//! `(MaterialKind, emit_velocity)` render pipeline and
//! `node.gltf_texture_source`'s blit compute pipeline are both hand-written
//! `Option::get_or_insert_with` caches that compile a real Metal pipeline
//! lazily on first use — neither is touched by
//! `GeneratorRegistry::prewarm_all`'s existing startup pass, which only
//! builds each bundled generator's graph topology
//! (`PresetRuntime::from_def_with_device` never calls `run()`).
//!
//! The fix (this session) adds `RenderScene::prewarm_pipelines` and
//! `GltfTextureSource::prewarm_pipeline` — both asset-independent (fixed
//! shader source, no per-project data needed) — and calls them from
//! `GeneratorRegistry::prewarm_all`, so both device-level pipeline caches are
//! warm before any project loads.
//!
//! This harness drives the REAL, unmodified live-render path
//! (`ContentPipeline::render_content` with `export_mode = false` — the exact
//! call `ContentThread::tick_frame` makes every frame) against a headless
//! `ContentThread` (reusing `journey_proof`'s construction, which already
//! solves the GPU-device-pointer-rebind hazard), with a layer running the
//! bundled `BlossomField` generator preset — the richest already-shipping
//! preset that wires `node.gltf_mesh_source` -> `node.render_scene` (as
//! object 1, textured via `node.gltf_texture_source` -> `base_color_map_1`)
//! against a REAL tracked fixture
//! (`tests/fixtures/gltf/apricot_blossom_cluster_lod.glb`, referenced by the
//! preset's own bundled `modelPath` default — no path patching needed, the
//! preset's hardcoded absolute path is the main checkout's copy, which
//! exists on this machine).
//!
//! With `MANIFOLD_RENDER_TRACE=1` set, any frame over 20ms prints a
//! `[RENDER_TRACE] ... generators=<ms> ...` breakdown to stderr — this test
//! doesn't assert on stdout/stderr capture (fragile across libtest
//! versions, same reasoning as BUG-035's harness); it's meant to be run with
//! `--nocapture` and the trace lines read by eye:
//!
//! ```text
//! MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app --features journey-proofs \
//!   --features gpu-proofs bug037_blossom_field_first_render_drives_120_frames -- --nocapture
//! ```
#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, Bpm, PresetTypeId, Seconds};
use manifold_playback::engine::TickContext;

use crate::headless_harness::headless_content_thread;

/// 640×360 — cheap enough to isolate the pipeline-compile stall from
/// resolution-driven render cost, big enough that `render_scene`'s MSAA
/// color+depth target allocation is representative of a real canvas.
const W: u32 = 640;
const H: u32 = 360;
const BPM: f32 = 120.0;
/// Long enough that the clip is still live for the whole `FRAMES` run (a
/// stopped clip falls back to the generator's cold-start thumbnail texture
/// instead of its live render — see BUG-035's harness for the same note).
const CLIP_BEATS: f64 = 64.0;
/// 2s @ 60fps: enough to cover the layer's first several rendered frames
/// (where the lazy pipeline compiles would land pre-fix) plus steady state
/// afterward, without paying for a long GPU-heavy run.
const FRAMES: u64 = 120;

fn blossom_field_generator_layer(index: i32) -> Layer {
    let mut layer = Layer::new("Blossom Field".to_string(), LayerType::Generator, index);
    let pid = PresetTypeId::from_string("BlossomField".to_string());
    layer.change_generator_type(pid);
    layer
        .clips
        .push(TimelineClip::new_generator(Beats(0.0), Beats(CLIP_BEATS)));
    layer
}

fn blossom_field_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(BPM);
    project.timeline.layers.push(blossom_field_generator_layer(0));
    project
}

/// Runs `FRAMES` real content-thread ticks with the `BlossomField` glTF scene
/// layer active from frame 0 — the layer's FIRST rendered frame is frame 0
/// itself. Not a pass/fail assertion (frame timing isn't a correctness
/// property `cfg(test)` can check reliably across machines) — run with
/// `MANIFOLD_RENDER_TRACE=1 -- --nocapture` and read the `[RENDER_TRACE]`
/// lines for `generators=` spikes on the early frames.
#[test]
fn bug037_blossom_field_first_render_drives_120_frames() {
    let project = blossom_field_project();

    let mut ct = headless_content_thread(project, W, H);

    ct.engine.play();

    let dt = 1.0 / 60.0;
    for frame in 0..FRAMES {
        let ctx = TickContext {
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(frame as f64 * dt),
            pre_render_dt: Seconds(dt),
            frame_count: frame,
            export_fixed_dt: Seconds::ZERO,
        };
        let tick_result = ct.engine.tick(ctx);
        ct.content_pipeline.render_content(
            &ct.gpu,
            &mut ct.engine,
            &tick_result,
            dt,
            frame,
            false, // export_mode = false: the live path BUG-037 lives on
            ct.editing_service.data_version(),
        );
    }
}
