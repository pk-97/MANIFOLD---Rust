//! PRESET_BROWSER_AUDITION P2 gate (f) driver — headless,
//! `MANIFOLD_RENDER_TRACE`-driven, same shape as `bug035_verify.rs`.
//!
//! Drives `FRAMES` real content-thread frames with the preset browser's
//! audition grid open against a live generator clip (the master tap is the
//! real composited frame). Timing is a fact, not an assertion — run:
//!
//! ```text
//! MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app \
//!   --features journey-proofs --features gpu-proofs \
//!   p2_audition_trace -- --nocapture
//! ```
//!
//! and read the `[RENDER_TRACE]` lines: `audition=` is the audition block's
//! share of any frame over 20 ms. The pass/fail assertion here covers the
//! correctness half — the grid actually rendered every frame the budget
//! allowed — plus the cold-touch count paid at open (counted, not hidden).

#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use manifold_core::preset_def::PresetKind;
use manifold_core::project::Project;
use manifold_core::{Beats, Bpm, PresetTypeId, Seconds};
use manifold_playback::engine::TickContext;

use crate::headless_harness::headless_content_thread;
use crate::journey_proof::star_field_generator_layer;

const BPM: f32 = 120.0;
const CLIP_BEATS: f64 = 96.0;
const FRAMES: u64 = 300; // 5s @ 60fps — steady state, several budget windows

fn audition_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(BPM);
    let mut layer = star_field_generator_layer(0);
    layer.clips[0].duration_beats = Beats(CLIP_BEATS);
    project.timeline.layers.push(layer);
    project
}

/// A browser-sized grid: 8 effects + 4 generators, master tap.
fn audition_items() -> Vec<(PresetTypeId, PresetKind)> {
    let effect = |id: &'static str| (PresetTypeId::new(id), PresetKind::Effect);
    let generator = |id: &'static str| (PresetTypeId::new(id), PresetKind::Generator);
    vec![
        effect("Invert"),
        effect("Mirror"),
        effect("Glitch"),
        effect("SoftFocus"),
        effect("Bloom"),
        effect("EdgeStretch"),
        effect("ColorGrade"),
        effect("Dither"),
        generator("StarField"),
        generator("Plasma"),
        generator("BlackHole"),
        generator("BasicShapes"),
    ]
}

#[test]
fn p2_audition_trace_drives_browser_open() {
    let project = audition_project();
    let mut ct = headless_content_thread(project, 320, 180);

    // Same public entry the `AuditionEnsureCells` command forwards to — the
    // pipeline builds the pool + the shared audition surface internally.
    let items = audition_items();
    manifold_core::cold_touch::reset_cold_touch_counts();
    ct.content_pipeline.audition_ensure_cells(
        items.clone(),
        manifold_renderer::audition::AuditionTapTarget::Master,
    );
    eprintln!(
        "[p2-trace] cold touches at open: {}",
        manifold_core::cold_touch::total_cold_touches()
    );
    ct.content_pipeline
        .audition_set_render_list(items.iter().map(|(id, _)| id.clone()).collect());

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
            false,
            ct.editing_service.data_version(),
        );
        ct.engine.reclaim_tick_result(tick_result);
    }

    // Correctness half: with a 12-cell list and K=2/frame, every frame is
    // under budget in this harness, so the whole grid must have rendered
    // repeatedly (each cell every 6 frames).
    let rendered = ct.content_pipeline.audition_renders_completed();
    assert!(
        rendered >= FRAMES * 2,
        "audition grid must render K=2 cells every frame the budget allowed; got {rendered}"
    );
}
