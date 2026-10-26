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

fn tick(
    engine: &mut PlaybackEngine,
    realtime: &mut f64,
    frame: &mut u64,
    frames: u32,
) -> manifold_playback::engine::TickResult {
    let ctx = TickContext {
        frames,
        realtime_now: Seconds(*realtime),
        frame_count: *frame,
    };
    let result = engine.tick(ctx);
    *realtime += engine.tick_dt().0;
    *frame += 1;
    result
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

/// section 5 / section 11: `correct_video_drift` re-seeks a player whose reported time
/// has drifted > 0.1s from the expected source time — but only on the
/// `video_sync_interval` cadence, not every frame.
///
/// The cadence gate (`current_time - last_sync_time >= video_sync_interval`)
/// is a single clock shared by ALL active clips, not a per-clip timer — a
/// single-clip synthetic project keeps that clock's phase fully
/// deterministic (nothing else perturbs `last_sync_time`), which a
/// multi-clip project like Right Where I Need You does not (background clips scheduled
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
    // gate itself; section 11's literal 2.0s default is asserted separately above.
    let interval = Seconds(0.5);
    engine.set_video_sync_interval(interval);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;

    // Tick until the clip is active (start_beat 0, so this settles fast).
    let mut clip_id: Option<manifold_core::ClipId> = None;
    for _ in 0..30 {
        let result = tick(&mut engine, &mut realtime, &mut frame, 1);
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
        tick(&mut engine, &mut realtime, &mut frame, 1);
    }
    let t0 = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);

    // A short window (well under the interval) must show no correction —
    // rules out "corrects every tick" regardless of the configured cadence.
    for _ in 0..3 {
        tick(&mut engine, &mut realtime, &mut frame, 1);
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
        tick(&mut engine, &mut realtime, &mut frame, 1);
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

/// section 5: a custom-loop-duration clip restarts at its `in_point` once the
/// player's reported time reaches `in_point + loop_len_sec` —
/// `check_custom_loop_boundaries`, driven by the real engine.
#[test]
fn custom_loop_boundary_restarts_clip_at_loop_in() {
    let project = looping_project();
    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;

    // Tick until the clip is active and StubRenderer reports it ready.
    let mut clip_id: Option<manifold_core::ClipId> = None;
    for _ in 0..30 {
        let result = tick(&mut engine, &mut realtime, &mut frame, 1);
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
/// `start_clip` must stamp `recently_started_times` with the engine's real
/// wall clock (`last_realtime_now`), never a zero epoch. With P1 reconcile
/// running every tick, a clip under a stopped playhead starts on the first
/// tick; that tick stamps the gate with the wall clock it carries.
#[test]
fn clip_start_anchors_recently_started_gate_on_real_clock_not_zero_epoch() {
    let project = single_video_clip_project(120.0);
    // Capture the id now — TimelineClip::default() auto-generates it, and
    // this is the exact clip the engine will start.
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut engine = create_engine();
    engine.initialize(project);

    // Drive the first stopped tick with a wall clock well past the 0.1s
    // gate window (section 11 RECENTLY_STARTED_TIME). P1 makes reconcile
    // unconditional, so the clip under the playhead starts here; the stamp
    // must use `last_realtime_now` (set from `ctx.realtime_now`), not zero.
    let clock_at_start = 2.0;
    let ctx = TickContext {
        frames: 1,
        realtime_now: Seconds(clock_at_start),
        frame_count: 0,
    };
    let _ = engine.tick(ctx);

    let stamped = engine
        .recently_started_time(&clip_id)
        .expect("start_clip must stamp recently_started_times when the clip starts");
    assert_eq!(
        stamped, clock_at_start,
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
    let ready = engine.filter_ready_clips(Seconds(1.0 / 60.0));
    assert!(
        !ready.iter().any(|r| r.clip_id == clip_id),
        "the just-started clip must be excluded from filter_ready_clips \
         during its first-frame settle window — this is the gate a zero \
         epoch silently defeats"
    );
}

/// A video renderer whose clips stay not-ready until the test releases them,
/// like a real decoder that is still opening the file. Wraps `StubRenderer`,
/// which is ready the moment a clip starts and so hides any bug that lives in
/// the gap between start and first frame.
struct DeferredReadyVideo {
    inner: StubRenderer,
    held: std::collections::HashSet<manifold_core::ClipId>,
}

impl DeferredReadyVideo {
    fn new() -> Self {
        Self {
            inner: StubRenderer::new_video(),
            held: std::collections::HashSet::new(),
        }
    }
}

impl ClipRenderer for DeferredReadyVideo {
    fn can_handle(&self, clip: &manifold_core::clip::TimelineClip) -> bool {
        self.inner.can_handle(clip)
    }
    fn start_clip(
        &mut self,
        clip: &manifold_core::clip::TimelineClip,
        current_time: Seconds,
        layers: &[manifold_core::layer::Layer],
        layer_index: i32,
        fire_clip_edge: bool,
    ) -> bool {
        self.held.insert(clip.id.clone());
        self.inner
            .start_clip(clip, current_time, layers, layer_index, fire_clip_edge)
    }
    fn stop_clip(&mut self, clip_id: &str) {
        self.held.remove(clip_id);
        self.inner.stop_clip(clip_id);
    }
    fn release_all(&mut self) {
        self.held.clear();
        self.inner.release_all();
    }
    fn is_clip_ready(&self, clip_id: &str) -> bool {
        !self.held.contains(clip_id) && self.inner.is_clip_ready(clip_id)
    }
    fn is_active(&self, clip_id: &str) -> bool {
        self.inner.is_active(clip_id)
    }
    fn is_clip_playing(&self, clip_id: &str) -> bool {
        self.inner.is_clip_playing(clip_id)
    }
    fn needs_prepare_phase(&self) -> bool {
        self.inner.needs_prepare_phase()
    }
    fn needs_drift_correction(&self) -> bool {
        self.inner.needs_drift_correction()
    }
    fn needs_pending_pause(&self) -> bool {
        self.inner.needs_pending_pause()
    }
    fn get_clip_playback_time(&self, clip_id: &str) -> f32 {
        self.inner.get_clip_playback_time(clip_id)
    }
    fn get_clip_media_length(&self, clip_id: &str) -> f32 {
        self.inner.get_clip_media_length(clip_id)
    }
    fn resume_clip(&mut self, clip_id: &str) {
        self.inner.resume_clip(clip_id);
    }
    fn pause_clip(&mut self, clip_id: &str) {
        self.inner.pause_clip(clip_id);
    }
    fn seek_clip(&mut self, clip_id: &str, video_time: f32) {
        self.inner.seek_clip(clip_id, video_time);
    }
    fn set_clip_looping(&mut self, clip_id: &str, looping: bool) {
        self.inner.set_clip_looping(clip_id, looping);
    }
    fn set_clip_playback_rate(&mut self, clip_id: &str, rate: f32) {
        self.inner.set_clip_playback_rate(clip_id, rate);
    }
    fn pre_render(&mut self, time: Seconds, beat: Beats, dt: f32) {
        self.inner.pre_render(time, beat, dt);
    }
    fn resize(&mut self, width: i32, height: i32) {
        self.inner.resize(width, height);
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// BUG-mmct0.1 (new video clips never seek to their start): a video clip
/// started while playing must play from its in-point plus the time since its
/// start once the decoder is ready, on the tick it becomes ready, not two
/// seconds later when drift correction comes round. A trimmed clip or a split
/// tail otherwise shows the head of the file first.
#[test]
fn new_video_clip_seeks_to_in_point_when_ready() {
    let in_point = 3.0_f32;
    let mut project = single_video_clip_project(120.0);
    project.timeline.layers[0].clips[0].in_point = Seconds(in_point as f64);
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let renderers: Vec<Box<dyn ClipRenderer>> = vec![
        Box::new(StubRenderer::new_generator()),
        Box::new(DeferredReadyVideo::new()),
    ];
    let mut engine = PlaybackEngine::new(renderers);
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;
    for _ in 0..10 {
        tick(&mut engine, &mut realtime, &mut frame, 1);
    }
    assert!(
        engine.renderers_mut()[1].is_active(&clip_id),
        "the clip under the playhead must have started"
    );

    // The decoder finishes opening; the next tick must seek the player.
    engine.renderers_mut()[1]
        .as_any_mut()
        .downcast_mut::<DeferredReadyVideo>()
        .expect("renderer index 1 is the deferred video renderer")
        .held
        .clear();
    tick(&mut engine, &mut realtime, &mut frame, 1);

    let expected = in_point + engine.current_time().as_f32();
    let played = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);
    assert!(
        (played - expected).abs() < 0.05,
        "a newly ready clip must seek to in_point + elapsed ({expected:.3}s); it sits at {played:.3}s"
    );
}

/// Runs an untrimmed video clip that starts at realtime 0 and whose decoder
/// is ready after `hold_ticks` ticks. Returns (realtime the decoder became
/// ready, realtime the clip first reached the compositor).
fn untrimmed_clip_ready_and_visible_times(hold_ticks: u32) -> (f64, f64) {
    let project = single_video_clip_project(120.0);
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let renderers: Vec<Box<dyn ClipRenderer>> = vec![
        Box::new(StubRenderer::new_generator()),
        Box::new(DeferredReadyVideo::new()),
    ];
    let mut engine = PlaybackEngine::new(renderers);
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;
    for _ in 0..hold_ticks {
        let result = tick(&mut engine, &mut realtime, &mut frame, 1);
        assert!(
            !result.ready_clips.iter().any(|r| r.clip_id == clip_id),
            "a clip whose decoder is still opening must not reach the compositor"
        );
    }
    assert!(
        engine.renderers_mut()[1].is_active(&clip_id),
        "the clip under the playhead must have started"
    );

    engine.renderers_mut()[1]
        .as_any_mut()
        .downcast_mut::<DeferredReadyVideo>()
        .expect("renderer index 1 is the deferred video renderer")
        .held
        .clear();
    let ready_at = realtime;
    for _ in 0..60 {
        let now = realtime;
        let result = tick(&mut engine, &mut realtime, &mut frame, 1);
        if result.ready_clips.iter().any(|r| r.clip_id == clip_id) {
            return (ready_at, now);
        }
    }
    panic!("the clip never reached the compositor within a second of its decoder being ready");
}

/// An untrimmed clip (in point 0) is already where it should start when its
/// decoder opens, so becoming ready must not restart its first-frame gate: it
/// shows at max(ready, start + 100 ms), never ready + 100 ms, or every video
/// cut lands late by the decoder's open time.
#[test]
fn untrimmed_clip_visible_at_later_of_ready_and_start_gate() {
    let gate = manifold_playback::engine::RECENTLY_STARTED_TIME as f64;
    let one_tick = 1.0 / 60.0 + 1e-6;
    // hold 10 ticks: ready after the start gate expired; hold 2: before it.
    for hold in [10_u32, 2] {
        let (ready_at, visible_at) = untrimmed_clip_ready_and_visible_times(hold);
        let expected = ready_at.max(gate);
        assert!(
            visible_at <= expected + one_tick,
            "hold {hold}: decoder ready at {ready_at:.3}s, start gate ends at {gate:.3}s, so \
             the clip must show by {expected:.3}s; it first showed at {visible_at:.3}s"
        );
    }
}

/// An untrimmed clip that starts mid-way, here because the transport jumped
/// into it, is not on its start when the decoder opens: it must still seek
/// to the playhead on the ready tick, not play the file head until drift
/// correction comes round.
#[test]
fn untrimmed_clip_started_mid_way_seeks_when_ready() {
    let project = single_video_clip_project(120.0);
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let renderers: Vec<Box<dyn ClipRenderer>> = vec![
        Box::new(StubRenderer::new_generator()),
        Box::new(DeferredReadyVideo::new()),
    ];
    let mut engine = PlaybackEngine::new(renderers);
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);
    engine.seek_to(Seconds(5.0));

    let mut realtime = 0.0_f64;
    let mut frame = 0_u64;
    for _ in 0..10 {
        tick(&mut engine, &mut realtime, &mut frame, 1);
    }
    assert!(
        engine.renderers_mut()[1].is_active(&clip_id),
        "the clip under the playhead must have started"
    );

    engine.renderers_mut()[1]
        .as_any_mut()
        .downcast_mut::<DeferredReadyVideo>()
        .expect("renderer index 1 is the deferred video renderer")
        .held
        .clear();
    tick(&mut engine, &mut realtime, &mut frame, 1);

    let expected = engine.current_time().as_f32();
    let played = engine.renderers_mut()[1].get_clip_playback_time(&clip_id);
    assert!(
        (played - expected).abs() < 0.05,
        "a clip started mid-way must seek to the playhead ({expected:.3}s) when ready; it sits at {played:.3}s"
    );
}
