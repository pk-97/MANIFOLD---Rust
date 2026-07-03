//! Engine-level integration tests for Session Mode P2
//! (`docs/SESSION_MODE_DESIGN.md`): proves the wiring through the real
//! `PlaybackEngine` — arrangement suppression (§6), the launch-from-stopped
//! path (§4), and scene launch/stop (§5) — on top of the pure resolution-math
//! unit tests in `src/session_state.rs`. Headless: `StubRenderer`, no GPU.

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::types::{LayerType, PlaybackState};
use manifold_core::{Beats, Bpm, ClipId, LayerId, SceneId, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::{ClipRenderer, StubRenderer};

fn create_engine() -> PlaybackEngine {
    let renderers: Vec<Box<dyn ClipRenderer>> = vec![
        Box::new(StubRenderer::new_generator()),
        Box::new(StubRenderer::new_video()),
    ];
    PlaybackEngine::new(renderers)
}

/// One generator layer ("L0"), 120 BPM, no timeline clips unless added.
fn make_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project
        .timeline
        .insert_layer(0, Layer::new("L0".into(), LayerType::Generator, 0));
    project
}

fn add_session_slot(project: &mut Project, layer_id: &LayerId, scene_id: &SceneId, seq: ClipSequence) {
    project
        .session
        .scenes
        .push(Scene { id: scene_id.clone(), name: "Scene".into(), color: None });
    project.session.slots.push(SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: seq,
        name: "Slot".into(),
        color: None,
    });
}

fn one_clip_sequence(length_beats: f64, clip_id: &str) -> ClipSequence {
    let mut clip = TimelineClip::new_generator(Beats(0.0), Beats(length_beats));
    clip.id = ClipId::new(clip_id);
    ClipSequence { length_beats: Beats(length_beats), clips: vec![clip] }
}

#[test]
fn launch_slot_from_stopped_starts_transport_and_plays_immediately() {
    let mut project = make_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-a");
    add_session_slot(&mut project, &layer_id, &scene_id, one_clip_sequence(4.0, "c1"));

    let mut engine = create_engine();
    engine.initialize(project);
    assert_eq!(engine.current_state(), PlaybackState::Stopped);

    engine.session_launch_slot(layer_id.clone(), scene_id.clone());

    // §4: a launch from stopped starts the transport and plays immediately —
    // not a dead click waiting for a quantize boundary that will never arrive
    // while stopped.
    assert_eq!(engine.current_state(), PlaybackState::Playing);
    assert_eq!(engine.active_clip_count(), 1, "session clip should be active immediately");
    assert!(engine.session_runtime().is_playing_layer(&layer_id));
    assert!(engine.session_runtime().is_overridden(&layer_id));
}

#[test]
fn session_override_suppresses_timeline_clip_on_same_layer() {
    let mut project = make_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    // A timeline clip covering beat 0..8 on the same layer.
    let mut tl_clip = TimelineClip::new_generator(Beats(0.0), Beats(8.0));
    tl_clip.id = ClipId::new("timeline-clip");
    project.timeline.layers[0].clips.push(tl_clip);

    let scene_id = SceneId::new("scene-a");
    add_session_slot(&mut project, &layer_id, &scene_id, one_clip_sequence(4.0, "session-clip"));

    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);
    engine.set_beat(Beats(1.0));
    engine.sync_clips_to_time();

    // Before any session launch: the timeline clip plays normally.
    assert_eq!(engine.active_clip_count(), 1);

    // Launch the session slot on the same layer — arrangement suppression
    // (§6) means the timeline clip stops and the session clip takes over,
    // even though the timeline clip is still "active" at this beat.
    engine.session_launch_slot(layer_id.clone(), scene_id.clone());
    engine.sync_clips_to_time();

    assert_eq!(engine.active_clip_count(), 1, "exactly one clip active — session, not both");

    // Back to arrangement: the timeline clip resumes, session content stops.
    engine.session_back_to_arrangement(Some(layer_id.clone()));
    engine.sync_clips_to_time();
    assert_eq!(engine.active_clip_count(), 1);
    assert!(!engine.session_runtime().is_overridden(&layer_id));
}

#[test]
fn transport_stop_clears_session_playback_but_not_override() {
    let mut project = make_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-a");
    add_session_slot(&mut project, &layer_id, &scene_id, one_clip_sequence(4.0, "c1"));

    let mut engine = create_engine();
    engine.initialize(project);
    engine.session_launch_slot(layer_id.clone(), scene_id.clone());
    assert_eq!(engine.active_clip_count(), 1);

    engine.stop();
    assert_eq!(engine.active_clip_count(), 0);
    assert!(!engine.session_runtime().is_playing_layer(&layer_id));
    assert!(
        engine.session_runtime().is_overridden(&layer_id),
        "session_override persists through transport stop (§4/§12)"
    );
}

#[test]
fn scene_launch_and_stop_matrix_end_to_end() {
    let mut project = make_project();
    project
        .timeline
        .insert_layer(1, Layer::new("L1".into(), LayerType::Generator, 1));
    let l0 = project.timeline.layers[0].layer_id.clone();
    let l1 = project.timeline.layers[1].layer_id.clone();
    let scene_a = SceneId::new("a");
    let scene_b = SceneId::new("b");
    // l0 has slots in both scenes; l1 has a slot only in scene a.
    add_session_slot(&mut project, &l0, &scene_a, one_clip_sequence(4.0, "l0-a"));
    add_session_slot(&mut project, &l0, &scene_b, one_clip_sequence(4.0, "l0-b"));
    add_session_slot(&mut project, &l1, &scene_a, one_clip_sequence(4.0, "l1-a"));

    let mut engine = create_engine();
    engine.initialize(project);

    engine.session_launch_scene(scene_a.clone());
    assert!(engine.session_runtime().is_playing_layer(&l0));
    assert!(engine.session_runtime().is_playing_layer(&l1));
    assert_eq!(engine.active_clip_count(), 2);

    // Launching scene b: l0 has a slot there (relaunches); l1 has none there,
    // so it gets a quantized stop (Ableton "stop other tracks").
    engine.session_launch_scene(scene_b.clone());
    // Quantize defaults to 1 bar (4 beats) and playback just started at beat 0,
    // so nothing is due yet on this same tick — advance to the boundary.
    engine.set_beat(engine.current_beat() + Beats(4.0));
    engine.sync_clips_to_time();

    assert!(engine.session_runtime().is_playing_layer(&l0));
    assert_eq!(engine.session_runtime().playing_scene(&l0), Some(&scene_b));
    assert!(!engine.session_runtime().is_playing_layer(&l1));
    assert!(engine.session_runtime().is_overridden(&l1), "l1 goes black, no arrangement fallback");

    engine.session_stop_all();
    engine.set_beat(engine.current_beat() + Beats(4.0));
    engine.sync_clips_to_time();
    assert_eq!(engine.session_runtime().playing_count(), 0);
}

#[test]
fn tick_does_not_panic_with_session_runtime_wired() {
    let mut project = make_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let scene_id = SceneId::new("scene-a");
    add_session_slot(&mut project, &layer_id, &scene_id, one_clip_sequence(1.0, "c1"));

    let mut engine = create_engine();
    engine.initialize(project);
    engine.session_launch_slot(layer_id, scene_id);

    let dt = 1.0 / 60.0;
    let mut realtime = 0.0;
    // 1-beat loop at 120 BPM wraps roughly every 0.5s — well over 120 frames
    // guarantees several wrap-restart boundaries are crossed live via tick().
    for i in 0..120 {
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
    assert_eq!(engine.active_clip_count(), 1, "the looping session clip stays active across wraps");
}
