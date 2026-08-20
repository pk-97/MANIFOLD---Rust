use manifold_core::types::PlaybackState;
use manifold_core::{Beats, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::StubRenderer;

fn fixture_path(name: &str) -> std::path::PathBuf {
    // The `.manifold` fixtures are gitignored (large personal projects), so a
    // `git worktree` checkout doesn't contain them. Resolve to the MAIN working
    // tree: `--git-common-dir` points at the primary repo's `.git`, whose parent
    // is the main checkout where the fixtures live — so these tests RUN from a
    // worktree instead of panicking with a confusing file-not-found. Falls back
    // to the crate-relative path (the main checkout, or if git isn't reachable).
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        && out.status.success()
        && let Ok(common) =
            std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).canonicalize()
        && let Some(main_root) = common.parent()
    {
        let candidate = main_root.join("tests/fixtures").join(name);
        if candidate.exists() {
            return candidate;
        }
    }
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

/// A fixture-free project with one video layer owning one generator clip at beat 0.
/// Used by the stopped-engine reconciliation cure-test.
fn project_with_clip_at_beat_zero() -> manifold_core::project::Project {
    let mut project = manifold_core::project::Project::default();
    let mut layer = manifold_core::layer::Layer::new(
        "Test".to_string(),
        manifold_core::types::LayerType::Video,
        0,
    );
    layer.clips.push(manifold_core::clip::TimelineClip::new_generator(
        manifold_core::Beats::ZERO,
        manifold_core::Beats(4.0),
    ));
    project.timeline.layers.push(layer);
    project
}

/// P3 helper: layer 0 has one generator clip at beats 0..4; layer 1 is empty.
/// The clip starts on layer 0 and the test drags it to layer 1.
fn project_with_draggable_clip() -> manifold_core::project::Project {
    let mut project = manifold_core::project::Project::default();
    let mut layer0 = manifold_core::layer::Layer::new(
        "Layer 0".to_string(),
        manifold_core::types::LayerType::Video,
        0,
    );
    layer0.clips.push(manifold_core::clip::TimelineClip::new_generator(
        manifold_core::Beats::ZERO,
        manifold_core::Beats(4.0),
    ));
    project.timeline.layers.push(layer0);
    project.timeline.layers.push(manifold_core::layer::Layer::new(
        "Layer 1".to_string(),
        manifold_core::types::LayerType::Video,
        1,
    ));
    project
}

/// P3 helper: move layer 0's first clip into layer 1 (a drag across layers),
/// keeping the clip id. Returns the clip id.
fn drag_clip_to_layer_1(engine: &mut PlaybackEngine) -> manifold_core::ClipId {
    let project = engine.project_mut().expect("project");
    let mut clip = project.timeline.layers[0].clips.remove(0);
    clip.layer_id = project.timeline.layers[1].layer_id.clone();
    let id = clip.id.clone();
    project.timeline.layers[1].clips.push(clip);
    id
}

fn stub_gen(engine: &PlaybackEngine) -> &StubRenderer {
    engine.renderers()[0]
        .as_any()
        .downcast_ref::<StubRenderer>()
        .expect("renderer 0 is the generator stub")
}

fn tick_once(engine: &mut PlaybackEngine, realtime: f64, frame: u64) {
    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(realtime),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: frame,
        export_fixed_dt: Seconds(0.0),
    };
    let _ = engine.tick(ctx);
}

/// P3 (BUG-2z07 (layer-drag staleness)): an active clip dragged to another
/// layer is healed by an edge-suppressed stop+start in the same reconcile —
/// the destination layer sees a rebind, never a trigger. Stopped transport.
#[test]
fn drag_active_clip_across_layers_rebinds() {
    let engine = &mut create_engine();
    engine.initialize(project_with_draggable_clip());
    tick_once(engine, 0.0, 0);
    let clip_id = drag_clip_to_layer_1(engine);

    // The drag produces no stop/start from the clip_id diff alone — the heal
    // must fire inside the reconcile.
    let healed = engine.sync_clips_to_time();

    assert!(healed, "a layer drag is a membership change");
    assert_eq!(
        stub_gen(engine).start_count_for(&clip_id),
        2,
        "the clip must be stopped and restarted (rebound to layer 1)"
    );
    assert_eq!(
        stub_gen(engine).last_edge_flag_for(&clip_id),
        Some(false),
        "the heal start carries fire_clip_edge=false — a drag is not a trigger"
    );
    assert!(
        engine.pending_clip_edge_layers().is_empty(),
        "heals emit no clip-edge for modulation"
    );
    assert_eq!(
        engine.last_active_clip_id_for(1).cloned().as_deref(),
        Some(clip_id.as_str()),
        "the destination layer's edge-diff state updates silently"
    );
}

/// P3: same heal while playing — the mechanism is transport-independent.
#[test]
fn drag_active_clip_across_layers_rebinds_while_playing() {
    let engine = &mut create_engine();
    engine.initialize(project_with_draggable_clip());
    engine.play();
    tick_once(engine, 0.0, 0);
    tick_once(engine, 1.0 / 60.0, 1);
    let clip_id = drag_clip_to_layer_1(engine);
    tick_once(engine, 2.0 / 60.0, 2);

    assert_eq!(stub_gen(engine).start_count_for(&clip_id), 2);
    assert_eq!(
        stub_gen(engine).last_edge_flag_for(&clip_id),
        Some(false)
    );
}

/// P3: an undragged active clip never heals — the reconcile stays a no-op.
#[test]
fn active_clip_without_layer_change_does_not_heal() {
    let engine = &mut create_engine();
    engine.initialize(project_with_draggable_clip());
    tick_once(engine, 0.0, 0);
    let clip_id = engine
        .project()
        .unwrap()
        .timeline
        .layers[0]
        .clips[0]
        .id
        .clone();

    let changed = engine.sync_clips_to_time();

    assert!(!changed, "no drag, no membership change");
    assert_eq!(stub_gen(engine).start_count_for(&clip_id), 1);
}

/// A fixture-free project with one video layer owning one enabled, maximally
/// sensitive clip trigger reading a single send's Full-band transient —
/// BUG-109's regression tests don't need a real fixture, just the minimal
/// shape `has_active_clip_triggers()` and `LiveTriggerState::evaluate*` walk.
fn project_with_clip_trigger(sensitivity: f32) -> manifold_core::project::Project {
    let send = manifold_core::audio_setup::AudioSend::new("Kick");
    let send_id = send.id.clone();
    let mut project = manifold_core::project::Project::default();
    project.audio_setup.sends.push(send);

    let mut layer = manifold_core::layer::Layer::new(
        "Strobe".to_string(),
        manifold_core::types::LayerType::Video,
        0,
    );
    let mut cfg = manifold_core::audio_trigger::LayerClipTrigger::new(
        manifold_core::audio_mod::AudioModSource {
            send_id,
            feature: manifold_core::audio_mod::AudioFeature::new(
                manifold_core::audio_mod::AudioFeatureKind::Transients,
                manifold_core::audio_mod::AudioBand::Full,
            ),
        },
    );
    cfg.enabled = true;
    cfg.shape.sensitivity = sensitivity;
    cfg.shape.attack_ms = 0.0;
    cfg.shape.release_ms = 0.0;
    layer.clip_triggers.push(cfg);
    project.timeline.layers.push(layer);
    project
}

/// A snapshot with one send whose Full-band transient is hot (well above the
/// fixed 0.5 fire edge).
fn hot_snapshot() -> manifold_core::audio_features::AudioFeatureSnapshot {
    let mut f = manifold_core::SendFeatures::default();
    f.bands[manifold_core::audio_mod::AudioBand::Full.index()].transients = 0.9;
    manifold_core::audio_features::AudioFeatureSnapshot { sends: vec![f] }
}

/// BUG-109 section 7.1 item 1: P3c's per-branch `FireMeterCapture` reset ran AFTER
/// `tick_playing`'s step 3b had already pushed the clip-trigger level,
/// wiping it every playing tick. The fix moved the reset to the top of
/// `tick()`, once, before either branch's evaluators run.
#[test]
fn playing_tick_leaves_the_clip_trigger_level_in_fire_meters() {
    let project = project_with_clip_trigger(1.0);
    let key = manifold_core::audio_trigger::fire_meter_key_for_clip_trigger(
        project.timeline.layers[0].layer_id.as_str(),
        0u64,
    );

    let mut engine = create_engine();
    engine.initialize(project);
    // `tick_audio_triggers` (step 3b) no-ops without a live clip manager —
    // real app startup always sets one; construct the default here so the
    // test exercises the same evaluator path a live session does.
    engine.set_live_clip_manager(manifold_playback::live_clip_manager::LiveClipManager::new());
    engine.set_state(PlaybackState::Playing);
    *engine.audio_snapshot_mut() = hot_snapshot();

    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(0.0),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: 0,
        export_fixed_dt: Seconds(0.0),
    };
    let _ = engine.tick(ctx);

    let level = engine.fire_meters().get(key);
    assert!(
        level.is_some_and(|l| l >= 0.5),
        "playing tick must leave the clip trigger's level in fire_meters, got {level:?}"
    );
}

/// BUG-109 section 7.1 item 2: while stopped, clip triggers must still push their
/// shaped level (a performer tuning at soundcheck needs to see it move) but
/// must never fire a clip.
#[test]
fn stopped_tick_pushes_the_level_and_fires_no_clip() {
    let project = project_with_clip_trigger(1.0);
    let key = manifold_core::audio_trigger::fire_meter_key_for_clip_trigger(
        project.timeline.layers[0].layer_id.as_str(),
        0u64,
    );

    let mut engine = create_engine();
    engine.initialize(project);
    // Engine starts Stopped (see engine_initializes_with_project below).
    *engine.audio_snapshot_mut() = hot_snapshot();

    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(0.0),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: 0,
        export_fixed_dt: Seconds(0.0),
    };
    let result = engine.tick(ctx);

    let level = engine.fire_meters().get(key);
    assert!(
        level.is_some_and(|l| l >= 0.5),
        "stopped tick must still show the shaped signal for soundcheck tuning, got {level:?}"
    );
    assert!(
        result.ready_clips.is_empty(),
        "a stopped tick must never fire a clip trigger"
    );
    assert_eq!(
        engine.active_clip_count(),
        0,
        "a stopped tick must never start a clip"
    );
}

#[test]
fn engine_initializes_with_project() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();

    engine.initialize(project);

    assert_eq!(engine.current_state(), PlaybackState::Stopped);
    assert_eq!(engine.current_time(), Seconds(0.0));
    assert_eq!(engine.current_beat(), Beats(0.0));
    assert!((engine.get_timeline_fallback_bpm() - 138.0).abs() < 0.01);
}

#[test]
fn stopped_engine_activates_clip_under_playhead() {
    let project = project_with_clip_at_beat_zero();
    let mut engine = create_engine();
    engine.initialize(project);

    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(0.0),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: 0,
        export_fixed_dt: Seconds(0.0),
    };

    let result = engine.tick(ctx);
    assert!(
        !result.ready_clips.is_empty() || engine.active_clip_count() > 0,
        "A stopped engine must reconcile every tick: the clip under the playhead should become active"
    );
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
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(realtime),
            pre_render_dt: Seconds(dt),
            frame_count: i as u64,
            export_fixed_dt: Seconds(0.0),
        };
        let _ = engine.tick(ctx);
        realtime += dt;
    }

    // After 1 second at 138 BPM, should be at ~2.3 beats (138/60 = 2.3)
    let expected_beat = 138.0 / 60.0;
    assert!(
        (engine.current_beat().0 - expected_beat).abs() < 0.1,
        "After 1s at 138 BPM, expected ~{expected_beat} beats, got {}",
        engine.current_beat()
    );
    assert!(
        (engine.current_time().0 - 1.0).abs() < 0.02,
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
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(realtime),
            pre_render_dt: Seconds(dt),
            frame_count: i as u64,
            export_fixed_dt: Seconds(0.0),
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
    assert!(
        engine.active_clip_count() > 0,
        "Should have active clips at beat ~163"
    );
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
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(realtime),
            pre_render_dt: Seconds(dt),
            frame_count: i as u64,
            export_fixed_dt: Seconds(0.0),
        };
        let _result = engine.tick(ctx);
        realtime += dt;
    }

    // Just verify it doesn't panic and time advanced
    assert!(engine.current_time() > Seconds(0.0));
    assert!(engine.current_beat() > Beats(0.0));
}

#[test]
fn engine_seek_updates_beat() {
    let project = load_project("Burn V5.manifold");
    let mut engine = create_engine();
    engine.initialize(project);

    // Seek to a specific time
    engine.seek_to(Seconds(30.0));
    // At 138 BPM, 30s = 69 beats
    let expected_beat = 30.0 * 138.0 / 60.0;
    assert!(
        (engine.current_beat().0 - expected_beat).abs() < 0.1,
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
    let original_beat = 100.0_f32;
    let seconds = engine.beat_to_timeline_time(Beats::from_f32(original_beat));
    let roundtrip_beat = engine.time_to_timeline_beat(seconds);

    assert!(
        (roundtrip_beat.0 as f32 - original_beat).abs() < 0.01,
        "Beat→seconds→beat roundtrip failed: {original_beat} → {seconds}s → {roundtrip_beat}"
    );
}

#[test]
fn engine_waypoints_stress_test() {
    let path = fixture_path("WAYPOINTS.manifold");
    if !path.exists() {
        return;
    }

    let project = manifold_io::loader::load_project(&path).unwrap();
    // Original 2311 clips; 295 overlapping clips removed on load repair.
    assert_eq!(project.timeline.total_clip_count(), 2016);

    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0;
    let mut total_ready = 0usize;

    // Tick 500 frames (~8.3 seconds)
    for i in 0..500 {
        let ctx = TickContext {
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(realtime),
            pre_render_dt: Seconds(dt),
            frame_count: i as u64,
            export_fixed_dt: Seconds(0.0),
        };
        let result = engine.tick(ctx);
        total_ready += result.ready_clips.len();
        realtime += dt;
    }

    assert!(
        engine.current_time() > Seconds(8.0),
        "Should have ticked ~8.3 seconds"
    );
    // WAYPOINTS has clips starting early in the timeline, so we should have seen some
    assert!(
        total_ready > 0,
        "WAYPOINTS should have active clips in the first 8 seconds"
    );
}

/// P2 helper: two video layers, each with a generator clip spanning beats 0..4.
/// Layer 0 is the top layer (index 0), layer 1 is below it (index 1).
fn project_with_two_video_layers() -> manifold_core::project::Project {
    let mut project = manifold_core::project::Project::default();
    for i in 0..2 {
        let mut layer = manifold_core::layer::Layer::new(
            format!("Layer {i}"),
            manifold_core::types::LayerType::Video,
            i,
        );
        layer.clips.push(manifold_core::clip::TimelineClip::new_generator(
            manifold_core::Beats::ZERO,
            manifold_core::Beats(4.0),
        ));
        project.timeline.layers.push(layer);
    }
    project
}

/// P2 helper: one audio layer + one video layer, each with a clip at beat 0.
fn project_with_audio_and_video_layers() -> manifold_core::project::Project {
    let mut project = manifold_core::project::Project::default();

    let mut audio = manifold_core::layer::Layer::new_audio("Audio".into(), 0);
    audio.clips.push(manifold_core::clip::TimelineClip::new_audio(
        "dummy.wav".to_string(),
        manifold_core::Beats::ZERO,
        manifold_core::Beats(4.0),
        manifold_core::Seconds::ZERO,
        manifold_core::Seconds(4.0),
    ));
    project.timeline.layers.push(audio);

    let mut video = manifold_core::layer::Layer::new(
        "Video".into(),
        manifold_core::types::LayerType::Video,
        1,
    );
    video.clips.push(manifold_core::clip::TimelineClip::new_generator(
        manifold_core::Beats::ZERO,
        manifold_core::Beats(4.0),
    ));
    project.timeline.layers.push(video);

    project
}

/// P2: muting a layer removes it from the composite but keeps the clip active
/// in the engine (hot mute). Before P2 the timeline query filtered muted clips.
#[test]
fn muted_layer_clip_stays_active() {
    let mut project = project_with_two_video_layers();
    project.timeline.layers[0].is_muted = true;

    let mut engine = create_engine();
    engine.initialize(project);

    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(0.0),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: 0,
        export_fixed_dt: Seconds(0.0),
    };
    let _ = engine.tick(ctx);

    assert!(
        engine.active_clip_count() > 0,
        "muted layer's clip must stay active (hot mute)"
    );
}

/// P2: clip-level mute leaves the clip active in the engine but the clip is
/// marked muted in the ready list for the compositor.
#[test]
fn clip_muted_stays_active_hidden() {
    let mut project = project_with_two_video_layers();
    project.timeline.layers[0].clips[0].is_muted = true;

    let mut engine = create_engine();
    engine.initialize(project);

    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(0.0),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: 0,
        export_fixed_dt: Seconds(0.0),
    };
    let result = engine.tick(ctx);

    assert!(
        engine.active_clip_count() > 0,
        "muted clip must stay active (hot mute)"
    );
    assert!(
        result.ready_clips.iter().any(|c| c.is_muted),
        "ready list must carry the muted clip's is_muted flag"
    );
}

/// P2c: soloing an audio layer must not suppress video layer membership.
/// Before P2 the timeline query's `any_solo` spanned all layers.
#[test]
fn audio_solo_does_not_suppress_video() {
    let mut project = project_with_audio_and_video_layers();
    project.timeline.layers[0].is_solo = true; // audio layer soloed

    let mut engine = create_engine();
    engine.initialize(project);

    let ctx = TickContext {
        dt_seconds: Seconds(1.0 / 60.0),
        realtime_now: Seconds(0.0),
        pre_render_dt: Seconds(1.0 / 60.0),
        frame_count: 0,
        export_fixed_dt: Seconds(0.0),
    };
    let result = engine.tick(ctx);

    let video_layer_index = result
        .ready_clips
        .iter()
        .find(|c| c.layer_index == 1)
        .map(|c| c.layer_index);
    assert_eq!(
        video_layer_index,
        Some(1),
        "video clip must stay active when only an audio layer is soloed"
    );
}

/// D7a: a fully-muted paused rig idles — compositor_dirty becomes false after
/// the dirty deadline expires.
#[test]
fn all_muted_paused_rig_idles() {
    let mut project = project_with_two_video_layers();
    for layer in &mut project.timeline.layers {
        layer.is_muted = true;
    }

    let mut engine = create_engine();
    engine.initialize(project);
    engine.pause();

    let dt = 1.0 / 60.0;
    // Tick long enough for the compositor dirty deadline to expire.
    // Start realtime past zero so a few frames clear COMPOSITOR_DIRTY_TIME (0.05s).
    let start_rt = 1.0;
    for i in 0..10 {
        let ctx = TickContext {
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(start_rt + i as f64 * dt),
            pre_render_dt: Seconds(dt),
            frame_count: i as u64,
            export_fixed_dt: Seconds(0.0),
        };
        let result = engine.tick(ctx);
        if i >= 5 {
            assert!(
                !result.compositor_dirty,
                "fully-muted paused rig should idle after deadline, frame {i}"
            );
        }
    }
}
