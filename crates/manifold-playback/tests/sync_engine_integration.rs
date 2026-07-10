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

/// F4: `sync_clips_to_time`'s `start_clip` call must stamp every downstream
/// clip-start timing gate (`recently_started_times`, and — via the same
/// `realtime_now` parameter — the pending-pause deadline and
/// `mark_compositor_dirty`) with the real engine clock (`last_realtime_now`),
/// never a zero epoch. A zero epoch is silently "already expired" the moment
/// the engine has been running longer than the gate window, which defeats
/// the compositor-exclusion gate exactly when it matters: a clip launched
/// well into a set, not one launched at t=0.
///
/// Drives the real `PlaybackEngine` through `tick()` and `play()` — never a
/// fork of `sync_clips_to_time`'s logic.
#[test]
fn clip_start_anchors_recently_started_gate_on_real_clock_not_zero_epoch() {
    let project = single_video_clip_project(120.0);
    // Capture the id now — TimelineClip::default() auto-generates it, and
    // this is the exact clip the engine will start.
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut engine = create_engine();
    engine.initialize(project);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;

    // Advance the engine's wall clock well past the 0.1s gate window
    // (§11 RECENTLY_STARTED_TIME) while STOPPED. `tick()` stamps
    // `last_realtime_now` unconditionally at its top regardless of playback
    // state (engine.rs `last_realtime_now = ctx.realtime_now.0`), so by the
    // time `play()` is called below, the clock already reads ~2s — this is
    // the "engine has been running a while before this clip launches" case
    // the zero-epoch bug breaks. No clip is active yet (stopped, and this
    // clip's layer has no other clips), so this loop is inert apart from
    // advancing the clock.
    for _ in 0..120 {
        tick(&mut engine, &mut realtime, &mut frame, dt);
    }
    assert!(
        realtime > 1.9,
        "test setup: the engine wall clock must be well past the 0.1s gate \
         window before the clip starts, or this test can't distinguish the \
         fix from the bug"
    );

    // `play()` calls `sync_clips_to_time()` directly, not through `tick()` —
    // this is exactly the call path the bug lived on. (Engine starts
    // Stopped from `initialize()`; `play()` itself transitions state — don't
    // pre-set it, or `play()`'s own re-entrancy guard would no-op it.)
    let clock_at_play = engine.last_realtime_now();
    engine.play();

    let stamped = engine
        .recently_started_time(&clip_id)
        .expect("start_clip must stamp recently_started_times when the clip starts");
    assert_eq!(
        stamped, clock_at_play,
        "recently_started_times must be stamped with the real engine clock \
         (last_realtime_now), not a zero epoch"
    );
    assert!(
        stamped > 1.0,
        "sanity: the engine clock must actually be non-zero here — a test \
         that let this drift to 0.0 would pass even under the old bug"
    );

    // The compositor-exclusion gate must actually engage on this same tick:
    // filter_ready_clips must exclude the just-started clip for its first
    // 0.1s settle window. This is the assertion that WOULD HAVE FAILED under
    // the old `Seconds::ZERO` epoch: `recently_started_times` would read
    // 0.0, so `last_realtime_now (~2s) - 0.0 = ~2s`, which is nowhere near
    // `< 0.1` — the clip would not have been excluded, a flash on its first
    // frame.
    let ready = engine.filter_ready_clips(Seconds(dt));
    assert!(
        !ready.iter().any(|r| r.clip_id == clip_id),
        "the just-started clip must be excluded from filter_ready_clips \
         during its first-frame settle window — this is the gate a zero \
         epoch silently defeats"
    );
}
