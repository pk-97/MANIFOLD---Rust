use manifold_core::types::PlaybackState;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::StubRenderer;

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

fn load_project(name: &str) -> manifold_core::project::Project {
    let path = fixture_path(name);
    manifold_io::loader::load_project(&path)
        .unwrap_or_else(|e| panic!("Failed to load {name}: {e}"))
}

fn create_engine() -> PlaybackEngine {
    let renderers: Vec<Box<dyn manifold_playback::renderer::ClipRenderer>> = vec![
        Box::new(StubRenderer::new_generator()),
        Box::new(StubRenderer::new_video()),
    ];
    PlaybackEngine::new(renderers)
}

#[test]
fn engine_initializes_with_project() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();

    engine.initialize(project);

    assert_eq!(engine.current_state(), PlaybackState::Stopped);
    assert_eq!(engine.current_time(), 0.0);
    assert_eq!(engine.current_beat(), 0.0);
    assert!((engine.get_timeline_fallback_bpm() - 138.0).abs() < 0.01);
}

#[test]
fn engine_tick_while_stopped_has_no_active_clips() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);

    let ctx = TickContext {
        dt_seconds: 1.0 / 60.0,
        realtime_now: 0.0,
        pre_render_dt: 1.0 / 60.0,
        frame_count: 0,
        export_fixed_dt: 0.0,
    };

    let result = engine.tick(ctx);
    assert!(result.ready_clips.is_empty(), "No clips should be ready when stopped at beat 0");
}

#[test]
fn engine_advances_time_when_playing() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0;

    // Tick 60 frames (1 second)
    for i in 0..60 {
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: i,
            export_fixed_dt: 0.0,
        };
        engine.tick(ctx);
        realtime += dt;
    }

    // After 1 second at 138 BPM, should be at ~2.3 beats (138/60 = 2.3)
    let expected_beat = 138.0 / 60.0;
    assert!(
        (engine.current_beat() - expected_beat).abs() < 0.1,
        "After 1s at 138 BPM, expected ~{expected_beat} beats, got {}",
        engine.current_beat()
    );
    assert!(
        (engine.current_time() - 1.0).abs() < 0.02,
        "After 60 frames at 1/60, expected ~1.0s, got {}",
        engine.current_time()
    );
}

#[test]
fn engine_schedules_clips_at_correct_beats() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0;
    let mut ever_had_ready_clips = false;

    // Tick through timeline — the first clip starts around beat 162
    // At 138 BPM, beat 162 ≈ 70.4 seconds = ~4226 frames
    // Let's tick to beat 163 to ensure we're in range
    let target_seconds = 163.0 * 60.0 / 138.0; // ~70.87s
    let num_frames = (target_seconds / dt) as i32;

    for i in 0..num_frames {
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: i,
            export_fixed_dt: 0.0,
        };
        let result = engine.tick(ctx);
        if !result.ready_clips.is_empty() {
            ever_had_ready_clips = true;
        }
        realtime += dt;
    }

    assert!(
        ever_had_ready_clips,
        "Engine should have scheduled clips during the timeline (ticked to beat ~163)"
    );
    assert!(engine.active_clip_count() > 0,
        "Should have active clips at beat ~163");
}

#[test]
fn engine_tick_1000_frames_no_panic() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0;

    for i in 0..1000 {
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: i,
            export_fixed_dt: 0.0,
        };
        let _result = engine.tick(ctx);
        realtime += dt;
    }

    // Just verify it doesn't panic and time advanced
    assert!(engine.current_time() > 0.0);
    assert!(engine.current_beat() > 0.0);
}

#[test]
fn engine_seek_updates_beat() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);

    // Seek to a specific time
    engine.seek_to(30.0);
    // At 138 BPM, 30s = 69 beats
    let expected_beat = 30.0 * 138.0 / 60.0;
    assert!(
        (engine.current_beat() - expected_beat).abs() < 0.1,
        "After seek to 30s at 138 BPM, expected ~{expected_beat} beats, got {}",
        engine.current_beat()
    );
}

#[test]
fn engine_beat_time_conversion_roundtrip() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);

    // Test beat → seconds → beat roundtrip
    let original_beat = 100.0;
    let seconds = engine.beat_to_timeline_time(original_beat);
    let roundtrip_beat = engine.time_to_timeline_beat(seconds);

    assert!(
        (roundtrip_beat - original_beat).abs() < 0.01,
        "Beat→seconds→beat roundtrip failed: {original_beat} → {seconds}s → {roundtrip_beat}"
    );
}

#[test]
fn engine_waypoints_stress_test() {
    let path = fixture_path("WAYPOINTS.manifold");
    if !path.exists() { return; }

    let project = manifold_io::loader::load_project(&path).unwrap();
    assert_eq!(project.timeline.total_clip_count(), 2311);

    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0;
    let mut total_ready = 0usize;

    // Tick 500 frames (~8.3 seconds)
    for i in 0..500 {
        let ctx = TickContext {
            dt_seconds: dt,
            realtime_now: realtime,
            pre_render_dt: dt as f32,
            frame_count: i,
            export_fixed_dt: 0.0,
        };
        let result = engine.tick(ctx);
        total_ready += result.ready_clips.len();
        realtime += dt;
    }

    assert!(engine.current_time() > 8.0, "Should have ticked ~8.3 seconds");
    // WAYPOINTS has clips starting early in the timeline, so we should have seen some
    assert!(total_ready > 0, "WAYPOINTS should have active clips in the first 8 seconds");
}
