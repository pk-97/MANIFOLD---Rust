//! BUG-035 (authoring-hitch) regression guard — headless, `MANIFOLD_RENDER_TRACE`-driven.
//!
//! `docs/BUG_BACKLOG.md`'s BUG-035 entry pinned the root cause to
//! `content_pipeline.rs`'s clip-atlas disk-persist debounce: every
//! `CLIP_ATLAS_SAVE_DEBOUNCE` (300 frames, ~5s) cycle, the old code called
//! `ReadbackRequest::try_read()` on the completed persist readback — a
//! scalar, per-pixel, per-channel f16→u8 convert over the FULL 8192×1152
//! clip atlas (9.4M pixels), inline on the content thread. Measured ~58ms.
//!
//! The fix (this session) switches the persist path to
//! `try_read_packed()` (a plain memcpy) and moves the f16→u8 convert +
//! per-cell slice into the existing clip-thumb disk worker thread via
//! `ClipThumbCache::store_atlas`, so the content thread never does
//! O(atlas-surface) CPU work.
//!
//! This harness drives the REAL, unmodified live-render path
//! (`ContentPipeline::render_content` with `export_mode = false` — the exact
//! call `ContentThread::tick_frame` makes every frame) against a headless
//! `ContentThread` (reusing `journey_proof`'s construction, which already
//! solves the GPU-device-pointer-rebind hazard), with a visible clip on the
//! clip atlas long enough to cross at least two debounce cycles. With
//! `MANIFOLD_RENDER_TRACE=1` set, any frame over 20ms prints a
//! `[RENDER_TRACE] ... clip_atlas=<ms> ...` breakdown to stderr — this test
//! doesn't assert on stdout/stderr capture (fragile across libtest versions);
//! it's meant to be run with `--nocapture` and the trace lines read by eye,
//! same as the original diagnosis. See the BUG-035 backlog entry for the
//! recorded before/after trace excerpts.
#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use manifold_core::project::Project;
use manifold_core::{Beats, Bpm, Seconds};
use manifold_playback::engine::TickContext;

use crate::headless_harness::headless_content_thread;
use crate::journey_proof::star_field_generator_layer;

/// 320×180 keeps the render cheap — this harness is about the clip-atlas
/// persist path, not render cost. 96 beats at 120 BPM covers ~96s of
/// timeline, far more than the ~15s / 900-frame run below needs, so the
/// clip never stops mid-run (a stopped clip would fall back to the
/// generator's cold-start thumbnail texture instead of its live texture —
/// still atlas-visible, but not the steady-state case being verified).
const BPM: f32 = 120.0;
const CLIP_BEATS: f64 = 96.0;
const FRAMES: u64 = 900; // 15s @ 60fps — 3 full CLIP_ATLAS_SAVE_DEBOUNCE (300-frame) cycles

fn atlas_persist_project() -> (Project, manifold_core::ClipId) {
    let mut project = Project::default();
    project.settings.bpm = Bpm(BPM);
    let mut layer = star_field_generator_layer(0);
    // `star_field_generator_layer`'s stock clip is 8 beats (journey_proof's
    // own short click-track length) — stretch it to CLIP_BEATS so the clip
    // is still live (not fallen back to the generator's cold-start
    // thumbnail) for the whole FRAMES run.
    layer.clips[0].duration_beats = Beats(CLIP_BEATS);
    let clip_id = layer.clips[0].id.clone();
    project.timeline.layers.push(layer);
    (project, clip_id)
}

/// Runs `FRAMES` real content-thread ticks with a clip on the clip atlas.
/// Not a pass/fail assertion by itself (the spike is a *timing* fact,
/// invisible to a correctness check) — run with
/// `MANIFOLD_RENDER_TRACE=1 cargo test -p manifold-app --features journey-proofs
/// --features gpu-proofs bug035_clip_atlas_persist_drives_900_frames -- --nocapture`
/// and read the `[RENDER_TRACE]` lines for `clip_atlas=` spikes.
#[test]
fn bug035_clip_atlas_persist_drives_900_frames() {
    let (project, clip_id) = atlas_persist_project();

    let mut ct = headless_content_thread(project, 320, 180);

    // `journey_proof`'s headless construction deliberately skips every
    // IOSurface bridge/surface (export never reads them) — but the clip-atlas
    // *persist* path this test targets lives entirely downstream of
    // `fill_clip_atlas`'s `persistent` param, which is
    // `self.clip_atlas_persistent` and stays `None` (capture is a no-op)
    // until `set_clip_atlas_texture` installs it. Wire up the same single
    // shared surface `app.rs`'s `resumed()` builds (~app.rs:2080-2093,
    // ~2192-2200; BUG-119 replaced the old triple-buffer bridge here), just
    // without a window behind it — IOSurface is a kernel GPU-memory object,
    // not a display resource.
    let clip_atlas_surface = std::sync::Arc::new(crate::shared_texture::SharedAtlasSurface::new(
        crate::content_pipeline::CLIP_ATLAS_W,
        crate::content_pipeline::CLIP_ATLAS_H,
    ));
    let native_dev = ct.content_pipeline.native_device().expect("native device was just set");
    let clip_atlas_tex = unsafe { clip_atlas_surface.import_texture_native(native_dev) };
    ct.content_pipeline
        .set_clip_atlas_texture(clip_atlas_tex, std::sync::Arc::clone(&clip_atlas_surface));

    // Clip-atlas visibility is normally driven by the UI thread (the
    // clip-atlas panel reporting which cells are on-screen); this harness
    // sets it directly on the content-side pipeline, same public entry
    // point `ContentThread::tick_frame` would forward it through.
    ct.content_pipeline.set_clip_atlas_visible(vec![clip_id]);

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
            false, // export_mode = false: the live path BUG-035 lives on
            ct.editing_service.data_version(),
        );
    }
}
