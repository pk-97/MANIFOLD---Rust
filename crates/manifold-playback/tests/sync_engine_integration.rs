//! Engine-integration tests for the external-sync safety net (F3).
//!
//! These drive the REAL `PlaybackEngine` (never a fork of its sync logic)
//! through `tick()`, mirroring `engine_tick.rs`'s style. `StubRenderer`'s
//! honest behavior — it never advances `playback_time` on its own
//! (`pre_render` is a no-op) — is itself the "lie": once a player is
//! anchored, its reported position stays frozen while the engine's
//! expected source time keeps advancing, so drift accumulates exactly like
//! a real stalled/unresponsive player. No renderer changes were needed.

use manifold_core::project::Project;
use manifold_core::types::PlaybackState;
use manifold_core::units::Bpm;
use manifold_core::{Beats, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::{ClipRenderer, StubRenderer};

fn create_engine() -> PlaybackEngine {
    let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
        Box::new(StubRenderer::new_generator()),
        Box::new(StubRenderer::new_video()),
    ];
    PlaybackEngine::new(renderers)
}

fn tick(engine: &mut PlaybackEngine, realtime: &mut f64, frame: &mut u64, dt: f64) -> manifold_playback::engine::TickResult {
    let ctx = TickContext {
        dt_seconds: Seconds(dt),
        realtime_now: Seconds(*realtime),
        pre_render_dt: Seconds(dt),
        frame_count: *frame,
        export_fixed_dt: Seconds(0.0),
    };
    let result = engine.tick(ctx);
    *realtime += dt;
    *frame += 1;
    result
}

/// §11 `video_sync_interval` = 2.0s (asserted directly on the default
/// engine — this is the ruled-correct constant). The cadence *behavior*
/// below runs with a shortened interval purely so the test is fast; the
/// gate mechanism under test (`current_time - last_sync_time >=
/// video_sync_interval`) is identical either way.
#[test]
fn default_video_sync_interval_matches_threshold_table() {
    let engine = create_engine();
    assert_eq!(engine.video_sync_interval(), Seconds(2.0));
}

/// Build a minimal single-clip project: one video layer, one long
/// (never-ending during the test), non-looping clip starting at beat 0.
/// Non-120 BPM deliberately (the beats-primary trap).
fn single_video_clip_project(bpm: f32) -> Project {
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;

    let mut project = Project::default();
    project.settings.bpm = Bpm(bpm);
    let mut layer = Layer::new_video("Drift Layer".to_string(), 0);
    let clip = TimelineClip {
        video_clip_id: "stub.mp4".to_string(),
        layer_id: layer.layer_id.clone(),
        start_beat: Beats::ZERO,
        duration_beats: Beats(10_000.0),
        in_point: Seconds::ZERO,
        ..TimelineClip::default()
    };
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
    project
}

/// §5 / §11: `correct_video_drift` re-seeks a player whose reported time
/// has drifted > 0.1s from the expected source time — but only on the
/// `video_sync_interval` cadence, not every frame.
///
/// The cadence gate (`current_time - last_sync_time >= video_sync_interval`)
/// is a single clock shared by ALL active clips, not a per-clip timer — a
/// single-clip synthetic project keeps that clock's phase fully
/// deterministic (nothing else perturbs `last_sync_time`), which a
/// multi-clip fixture like Burn V5 does not (background clips scheduled
/// long before the one under test keep the cadence clock mid-cycle by the
/// time this clip starts, making "N frames after this clip appeared" an
/// unreliable proxy for "N frames after the last correction").
#[test]
fn drift_correction_reseeks_only_after_cadence_interval() {
    let project = single_video_clip_project(150.0); // deliberately non-120 BPM
    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);
    // Shrink the cadence for a fast test — the mechanism under test is the
    // gate itself; §11's literal 2.0s default is asserted separately above.
    let interval = Seconds(0.5);
    engine.set_video_sync_interval(interval);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;

    // Tick until the clip is active (start_beat 0, so this settles fast).
    let mut clip_id: Option<manifold_core::ClipId> = None;
    for _ in 0..30 {
        let result = tick(&mut engine, &mut realtime, &mut frame, dt);
        for r in &result.ready_clips {
            if engine.renderers_mut()[1].is_active(&r.clip_id) {
                clip_id = Some(r.clip_id.clone());
            }
        }
        if clip_id.is_some() {
            break;
        }
    }
    let clip_id = clip_id.expect("clip should become active immediately (start_beat 0)");

    // Let the one-time prepare-phase anchor seek settle before sampling a
    // reference point. (Deliberately NOT asserting where in the cadence
    // phase this lands — see the doc comment above.)
    for _ in 0..5 {
        tick(&mut engine, &mut realtime, &mut frame, dt);
    }
    let t0 = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);

    // A short window (well under the interval) must show no correction —
    // rules out "corrects every tick" regardless of the configured cadence.
    for _ in 0..3 {
        tick(&mut engine, &mut realtime, &mut frame, dt);
    }
    let just_after = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);
    assert_eq!(
        just_after, t0,
        "drift correction must not fire faster than the configured cadence"
    );

    // Ticking through a full interval (plus margin) guarantees crossing the
    // engine's next scheduled correction boundary regardless of phase —
    // StubRenderer's frozen playback_time will have drifted well past the
    // 0.1s threshold by then.
    let interval_frames = (interval.0 / dt).ceil() as u32 + 5;
    for _ in 0..interval_frames {
        tick(&mut engine, &mut realtime, &mut frame, dt);
    }
    let after_interval = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);
    assert!(
        after_interval > t0 + 0.1,
        "a drift-correction seek must have fired within one video_sync_interval \
         (t0={t0}, after_interval={after_interval})"
    );
}

/// Build a minimal single-clip project: one video layer, one looping clip
/// with a 2-beat custom loop window inside a much longer clip. Non-120 BPM
/// deliberately (the beats-primary trap).
fn looping_project() -> Project {
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;

    let mut project = Project::default();
    project.settings.bpm = Bpm(150.0);

    let mut layer = Layer::new_video("Loop Layer".to_string(), 0);
    let clip = TimelineClip {
        video_clip_id: "stub.mp4".to_string(),
        layer_id: layer.layer_id.clone(),
        start_beat: Beats::ZERO,
        duration_beats: Beats(1000.0),
        in_point: Seconds::ZERO,
        is_looping: true,
        loop_duration_beats: Beats(2.0), // loop every 2 beats @ 150 BPM = 0.8s
        ..TimelineClip::default()
    };
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
    project
}

/// §5: a custom-loop-duration clip restarts at its `in_point` once the
/// player's reported time reaches `in_point + loop_len_sec` —
/// `check_custom_loop_boundaries`, driven by the real engine.
#[test]
fn custom_loop_boundary_restarts_clip_at_loop_in() {
    let project = looping_project();
    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;

    // Tick until the clip is active and StubRenderer reports it ready.
    let mut clip_id: Option<manifold_core::ClipId> = None;
    for _ in 0..30 {
        let result = tick(&mut engine, &mut realtime, &mut frame, dt);
        for r in &result.ready_clips {
            if engine.renderers_mut()[1].is_active(&r.clip_id) {
                clip_id = Some(r.clip_id.clone());
            }
        }
        if clip_id.is_some() {
            break;
        }
    }
    let clip_id = clip_id.expect("looping clip should become active immediately (start_beat 0)");

    // Manually push the stub's reported playback_time past the loop
    // boundary (in_point 0.0 + loop_len 0.8s @ 150 BPM) — this is
    // StubRenderer's honest lie: nothing else moves playback_time, so we
    // set it directly to simulate "the player decoded up to the loop
    // point."
    {
        let video = engine.renderers_mut()[1]
            .as_any_mut()
            .downcast_mut::<StubRenderer>()
            .expect("renderer index 1 is the video StubRenderer");
        video.seek_clip(&clip_id, 0.85); // past the 0.8s boundary
    }

    engine.check_custom_loop_boundaries();

    let after = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);
    assert!(
        after < 0.85,
        "check_custom_loop_boundaries must restart the clip at in_point \
         (0.0) once playback_time crosses the loop boundary; got {after}"
    );
    assert_eq!(
        after, 0.0,
        "the loop restart seeks exactly to in_point (0.0 for this clip)"
    );
}
