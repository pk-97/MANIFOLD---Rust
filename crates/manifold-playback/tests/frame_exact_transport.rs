//! Frame-exact transport, engine level (docs/FRAME_EXACT_TRANSPORT_DESIGN.md
//! section 5, BUG-gqhep (live-transport-nondeterministic-frame-clock)).
//!
//! The same arrangement played twice must give the same values on the same
//! transport frame, whatever the Play-press phase, tick jitter or stalls.
//! Committed red while the engine added measured wall time; green once the
//! tick hands the engine grid frames and time comes from the frame index.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use manifold_core::audio_mod::{TriggerAction, WrapMode};
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::{
    AutomationLane, AutomationPoint, ParamEnvelope, ParameterDriver, PresetInstance, SegmentShape,
};
use manifold_core::layer::Layer;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::tempo::TempoMapConverter;
use manifold_core::types::{BeatDivision, DriverWaveform, PlaybackState, TempoPointSource};
use manifold_core::{Beats, Bpm, ClipId, LayerId, PresetTypeId, SceneId, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::external_follow::{ExternalSample, FollowProfile};
use manifold_playback::frame_grid::FrameGrid;
use manifold_playback::renderer::StubRenderer;

const FPS: f32 = 60.0;
/// Stopped ticks before Play, so the press lands between two jittered ticks.
const PRE_ROLL_TICKS: usize = 3;
/// 20 s at 60 fps: past the 128 → 141 change at beat 32 (frame 867).
const PLAYING_TICKS: usize = 1_200;
/// The jittered run's one stall: this playing tick arrives 3 frames late.
const STALL_AT_TICK: usize = 500;
/// The session launch is pressed right after the tick that reaches this frame.
const LAUNCH_AFTER_INDEX: i64 = 300;
const SEED: u64 = 0x5EED_F2A3_E1C7;

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn bundled_param(inst: &mut PresetInstance, id: &str, max: f32, whole_numbers: bool) {
    inst.params.push(Param::bundled(ParamSpecDef {
        tooltip: None,
        id: id.to_string(),
        name: id.to_string(),
        min: 0.0,
        max,
        default_value: 0.0,
        whole_numbers,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
        card_visible: true,
        material_role: None,
        limit: Default::default(),
        scene: Default::default(),
        unit: Default::default(),
    }));
}

fn point(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
    AutomationPoint {
        beat: Beats(beat),
        value,
        shape,
    }
}

fn clips(spans: &[(f64, f64)]) -> Vec<TimelineClip> {
    spans
        .iter()
        .map(|&(start, length)| TimelineClip::new_generator(Beats(start), Beats(length)))
        .collect()
}

struct Fixture {
    project: Project,
    session_layer: LayerId,
    scene: SceneId,
}

/// 150 BPM to beat 8 (frame 192), 128 to beat 32 (frame 867), then 141. At
/// 150 BPM every 1/8 beat sits exactly on a 60 fps frame, so the first
/// section is full of on-frame edges; the later sections mix on-frame tempo
/// boundaries with off-grid clip starts.
fn fixture() -> Fixture {
    let mut project = Project::default();
    project.settings.bpm = Bpm(150.0);
    project.settings.frame_rate = FPS;
    for (beat, bpm) in [(0.0, 150.0), (8.0, 128.0), (32.0, 141.0)] {
        project.tempo_map.add_or_replace_point(
            Beats(beat),
            Bpm(bpm),
            TempoPointSource::Manual,
            0.001,
        );
    }

    let mut main = Layer::new_generator("Main".into(), PresetTypeId::new("TestGen"), 0);
    let gp = main.gen_params_mut().unwrap();
    bundled_param(gp, "auto", 1.0, false);
    bundled_param(gp, "sync_lfo", 1.0, false);
    bundled_param(gp, "free_lfo", 1.0, false);
    bundled_param(gp, "level", 8.0, true);
    // Curve and step segments; the steps at 1.0, 1.5, 2.25, 8 and 32 sit on frames.
    gp.automation_lanes = Some(vec![AutomationLane {
        param_id: "auto".into(),
        enabled: true,
        points: vec![
            point(0.0, 0.1, SegmentShape::Curved(0.6)),
            point(1.0, 0.8, SegmentShape::Hold),
            point(1.5, 0.3, SegmentShape::Hold),
            point(2.25, 0.6, SegmentShape::Linear),
            point(8.0, 0.2, SegmentShape::Hold),
            point(12.0, 0.9, SegmentShape::Curved(-0.5)),
            point(20.0, 0.4, SegmentShape::Hold),
            point(32.0, 0.7, SegmentShape::Hold),
            point(36.0, 0.1, SegmentShape::Linear),
        ],
    }]);
    let mut free = ParameterDriver::new("free_lfo", BeatDivision::Quarter, DriverWaveform::Sine);
    free.free_period_beats = Some(1.37);
    gp.drivers = Some(vec![
        ParameterDriver::new("sync_lfo", BeatDivision::Sixteenth, DriverWaveform::Sine),
        free,
    ]);
    let mut envelope = ParamEnvelope::new("level");
    envelope.action = TriggerAction::Step {
        amount: 1.0,
        wrap: WrapMode::Wrap,
    };
    gp.envelopes = Some(vec![envelope]);
    // On-frame starts and ends every half beat at 150 BPM, then off-grid starts.
    let mut spans: Vec<(f64, f64)> = (0..16).map(|i| (i as f64 * 0.5, 0.375)).collect();
    spans.extend(
        [
            8.3, 9.17, 11.05, 13.333, 17.71, 21.9, 26.41, 30.07, 33.13, 35.55,
        ]
        .map(|s| (s, 0.6)),
    );
    main.clips = clips(&spans);

    // Tempo boundaries (8, 16, 32) and sub-beat starts as clip edges.
    let mut second = Layer::new_generator("Second".into(), PresetTypeId::new("TestGen"), 1);
    second.clips = clips(&[
        (0.125, 0.25),
        (3.0625, 0.5),
        (8.0, 1.0),
        (16.0, 2.0),
        (32.0, 1.0),
    ]);

    let session = Layer::new_generator("Session".into(), PresetTypeId::new("TestGen"), 2);
    let session_layer = session.layer_id.clone();
    let scene = SceneId::new("frame-exact-scene");
    project.session.scenes.push(Scene {
        id: scene.clone(),
        name: "Scene".into(),
        color: None,
    });
    project.session.slots.push(SessionSlot {
        layer_id: session_layer.clone(),
        scene_id: scene.clone(),
        name: "Loop".into(),
        color: None,
        sequence: ClipSequence {
            length_beats: Beats(2.0),
            clips: clips(&[(0.0, 0.5), (1.0, 0.25)]),
        },
    });

    project.timeline.layers = vec![main, second, session];
    Fixture {
        project,
        session_layer,
        scene,
    }
}

/// What one transport frame shows: the values the determinism claim covers
/// (D16).
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    time_bits: u64,
    beat_bits: u64,
    active_clips: Vec<usize>,
    param_bits: Vec<u32>,
    step_values: Vec<Option<u32>>,
    session: (Option<String>, usize, usize),
}

fn all_clip_ids(project: &Project) -> Vec<ClipId> {
    let timeline = project.timeline.layers.iter().flat_map(|l| l.clips.iter());
    let session = project
        .session
        .slots
        .iter()
        .flat_map(|s| s.sequence.clips.iter());
    timeline.chain(session).map(|c| c.id.clone()).collect()
}

fn capture(engine: &PlaybackEngine, clip_ids: &[ClipId], session_layer: &LayerId) -> Frame {
    let project = engine.project().expect("project");
    let renderer = &engine.renderers()[0];
    let instances = project
        .timeline
        .layers
        .iter()
        .filter_map(|l| l.gen_params());
    let mut param_bits = Vec::new();
    let mut step_values = Vec::new();
    for gp in instances {
        param_bits.extend(gp.params.iter().map(|p| p.value.to_bits()));
        step_values.extend(
            gp.envelopes
                .iter()
                .flatten()
                .map(|e| e.step_value.map(f32::to_bits)),
        );
    }
    let runtime = engine.session_runtime();
    Frame {
        time_bits: engine.current_time_double().to_bits(),
        beat_bits: engine.current_beat_f64().to_bits(),
        active_clips: (0..clip_ids.len())
            .filter(|&i| renderer.is_active(clip_ids[i].as_str()))
            .collect(),
        param_bits,
        step_values,
        session: (
            runtime
                .playing_scene(session_layer)
                .map(|s| format!("{s:?}")),
            runtime.pending_count(),
            runtime.playing_count(),
        ),
    }
}

enum Event {
    Tick(Instant),
    Press(Instant),
}

impl Event {
    fn at(&self) -> Instant {
        match self {
            Event::Tick(at) | Event::Press(at) => *at,
        }
    }
}

fn offset(base: Instant, seconds: f64) -> Instant {
    if seconds >= 0.0 {
        base + Duration::from_secs_f64(seconds)
    } else {
        base - Duration::from_secs_f64(-seconds)
    }
}

/// One run's events on the content tick grid anchored at `anchor`. Without a
/// seed: every tick on its deadline and Play right after the last stopped
/// tick. With a seed: each tick ±40% of a frame off its deadline, Play at a
/// random phase within a frame, and one tick 3 frames late.
fn schedule(anchor: Instant, seed: Option<u64>) -> Vec<Event> {
    let grid = FrameGrid::new(FPS, anchor);
    let step = 1.0 / f64::from(FPS);
    let mut rng = seed.map(Rng);
    let mut jitter = || rng.as_mut().map_or(0.0, |r| (r.unit() * 0.8 - 0.4) * step);
    let mut events = Vec::new();
    let mut deadline = 0i64;
    for _ in 0..PRE_ROLL_TICKS {
        deadline += 1;
        events.push(Event::Tick(offset(grid.deadline(deadline), jitter())));
    }
    let phase = seed.map_or(0.0, |s| Rng(s ^ 0xA5A5).unit() * step);
    events.push(Event::Press(offset(grid.deadline(deadline), phase)));
    for tick in 0..PLAYING_TICKS {
        deadline += if seed.is_some() && tick == STALL_AT_TICK {
            4
        } else {
            1
        };
        events.push(Event::Tick(offset(grid.deadline(deadline), jitter())));
    }
    // Commands drain at the start of a tick, so a press orders by its instant.
    events.sort_by_key(Event::at);
    events
}

/// Feed one run's events to the engine; `on_frame` sees each playing tick
/// with the transport frame index it rendered. Each tick hands the engine the
/// grid frames the D2 tick grid counted since the previous tick, as the
/// content thread's timer does, and the index is the engine clock's.
fn drive(
    engine: &mut PlaybackEngine,
    anchor: Instant,
    events: &[Event],
    mut on_frame: impl FnMut(&mut PlaybackEngine, i64),
) {
    let mut grid = FrameGrid::new(FPS, anchor);
    let mut frame_count = 0u64;
    for event in events {
        let now = match event {
            Event::Press(_) => {
                engine.play();
                continue;
            }
            Event::Tick(now) => *now,
        };
        let frames = grid.consume_at(now);
        let playing = engine.is_playing();
        let result = engine.tick(TickContext {
            frames,
            realtime_now: Seconds(now.saturating_duration_since(anchor).as_secs_f64()),
            frame_count,
        });
        engine.reclaim_tick_result(result);
        frame_count += 1;
        if playing {
            let index = engine.transport_frame();
            on_frame(engine, index);
        }
    }
}

fn run(seed: Option<u64>) -> BTreeMap<i64, Frame> {
    let Fixture {
        project,
        session_layer,
        scene,
    } = fixture();
    let clip_ids = all_clip_ids(&project);
    let mut engine = PlaybackEngine::new(vec![Box::new(StubRenderer::new_generator())]);
    engine.initialize(project);
    engine.session_set_quantize(Beats(4.0));
    let anchor = Instant::now();
    let events = schedule(anchor, seed);
    let mut frames = BTreeMap::new();
    let mut launched = false;
    drive(&mut engine, anchor, &events, |engine, index| {
        frames.insert(index, capture(engine, &clip_ids, &session_layer));
        if !launched && index >= LAUNCH_AFTER_INDEX {
            engine.session_launch_slot(session_layer.clone(), scene.clone());
            launched = true;
        }
    });
    frames
}

fn seconds(bits: u64) -> String {
    format!("{} (bits {bits:#018x})", f64::from_bits(bits))
}

/// The first transport frame both runs rendered where any value differs.
fn first_divergence(a: &BTreeMap<i64, Frame>, b: &BTreeMap<i64, Frame>) -> Option<String> {
    let (index, fa, fb) = a
        .iter()
        .filter_map(|(i, fa)| b.get(i).map(|fb| (i, fa, fb)))
        .find(|(_, fa, fb)| fa != fb)?;
    let field = if fa.time_bits != fb.time_bits {
        format!(
            "time {} vs {}",
            seconds(fa.time_bits),
            seconds(fb.time_bits)
        )
    } else if fa.beat_bits != fb.beat_bits {
        format!(
            "beat {} vs {}",
            seconds(fa.beat_bits),
            seconds(fb.beat_bits)
        )
    } else if fa.active_clips != fb.active_clips {
        format!(
            "active clips {:?} vs {:?}",
            fa.active_clips, fb.active_clips
        )
    } else if fa.param_bits != fb.param_bits {
        let p = fa
            .param_bits
            .iter()
            .zip(&fb.param_bits)
            .position(|(x, y)| x != y)
            .unwrap_or(0);
        format!(
            "param {p}: {} vs {}",
            f32::from_bits(fa.param_bits[p]),
            f32::from_bits(fb.param_bits[p])
        )
    } else if fa.step_values != fb.step_values {
        format!(
            "envelope steps {:?} vs {:?}",
            fa.step_values, fb.step_values
        )
    } else {
        format!("session {:?} vs {:?}", fa.session, fb.session)
    };
    let shared = a.keys().filter(|i| b.contains_key(i)).count();
    let differing = a
        .iter()
        .filter(|(i, fa)| b.get(i).is_some_and(|fb| fb != *fa))
        .count();
    Some(format!(
        "{differing} of {shared} shared transport frames differ; the first is frame {index}: {field}"
    ))
}

fn distinct<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> usize {
    let mut seen: Vec<T> = Vec::new();
    for v in values {
        if !seen.contains(&v) {
            seen.push(v);
        }
    }
    seen.len()
}

/// The steady run exercises everything the comparison claims to cover.
fn assert_not_vacuous(frames: &BTreeMap<i64, Frame>) {
    let last = frames.values().next_back().expect("frames");
    assert!(
        f64::from_bits(last.beat_bits) > 33.0,
        "the run must cross the 141 BPM change at beat 32"
    );
    let param = |p: usize| distinct(frames.values().map(move |f| f.param_bits[p]));
    assert!(param(0) >= 8, "automation must move: {} values", param(0));
    assert!(
        param(1) >= 100 && param(2) >= 100,
        "both LFOs must move: {} and {}",
        param(1),
        param(2)
    );
    let steps = distinct(frames.values().map(|f| f.step_values[0]));
    assert!(
        steps >= 8,
        "the clip-edge step envelope must advance: {steps} values"
    );
    let memberships = distinct(frames.values().map(|f| f.active_clips.len()));
    assert!(
        memberships >= 3,
        "clips must start and stop: {memberships} membership sizes"
    );
    let launched = frames
        .iter()
        .find(|(_, f)| f.session.2 > 0)
        .map(|(i, _)| *i);
    assert!(
        launched.is_some_and(|i| i > LAUNCH_AFTER_INDEX),
        "the quantised launch must wait for its bar: {launched:?}"
    );
}

#[test]
fn same_values_per_index_across_phase_jitter_and_stalls() {
    let steady = run(None);
    assert_eq!(steady.len(), PLAYING_TICKS, "one frame per steady tick");
    assert_not_vacuous(&steady);
    // The harness itself is repeatable: identical instants give identical frames.
    assert_eq!(
        first_divergence(&steady, &run(None)),
        None,
        "the steady run must repeat bit for bit"
    );

    let jittered = run(Some(SEED));
    let shared = steady.keys().filter(|i| jittered.contains_key(i)).count();
    assert!(
        shared + 3 >= PLAYING_TICKS,
        "the jittered run must render all but the stalled frames: {shared}"
    );
    if let Some(divergence) = first_divergence(&steady, &jittered) {
        panic!("same arrangement, different values per transport frame: {divergence}");
    }
}

// ─── Edge cases (section 5). `jump_over_two_frames_launches_and_resets` is D6, P3. ───

/// NoteOn is pressed right after the tick that reaches this frame.
const NOTE_AFTER_INDEX: i64 = 400;

fn engine_with(project: Project) -> PlaybackEngine {
    let mut engine = PlaybackEngine::new(vec![Box::new(StubRenderer::new_generator())]);
    engine.initialize(project);
    engine
}

fn tick(engine: &mut PlaybackEngine, frames: u32) {
    let result = engine.tick(TickContext {
        frames,
        ..TickContext::default()
    });
    engine.reclaim_tick_result(result);
}

/// One generator layer with `spans` as clips, at one tempo.
fn constant_tempo(bpm: f32, fps: f32, spans: &[(f64, f64)]) -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(bpm);
    project.settings.frame_rate = fps;
    project
        .tempo_map
        .add_or_replace_point(Beats::ZERO, Bpm(bpm), TempoPointSource::Manual, 0.001);
    let mut layer = Layer::new_generator("Gen".into(), PresetTypeId::new("TestGen"), 0);
    layer.clips = clips(spans);
    project.timeline.layers.push(layer);
    project
}

/// `constant_tempo` plus a session layer with one two-beat slot.
fn session_project(bpm: f32) -> (Project, LayerId, SceneId) {
    let mut project = constant_tempo(bpm, FPS, &[]);
    let session = Layer::new_generator("Session".into(), PresetTypeId::new("TestGen"), 1);
    let layer = session.layer_id.clone();
    let scene = SceneId::new("edge-scene");
    project.session.scenes.push(Scene {
        id: scene.clone(),
        name: "Scene".into(),
        color: None,
    });
    project.session.slots.push(SessionSlot {
        layer_id: layer.clone(),
        scene_id: scene.clone(),
        name: "Loop".into(),
        color: None,
        sequence: ClipSequence {
            length_beats: Beats(2.0),
            clips: clips(&[(0.0, 1.0)]),
        },
    });
    project.timeline.layers.push(session);
    (project, layer, scene)
}

fn is_active(engine: &PlaybackEngine, clip: &ClipId) -> bool {
    engine.renderers()[0].is_active(clip.as_str())
}

/// D11: a tempo edit while playing keeps the musical position. The index
/// moves to the first frame at or after the position's new time, with no hold.
#[test]
fn tempo_edit_during_play_keeps_beat() {
    let mut engine = engine_with(constant_tempo(150.0, FPS, &[]));
    engine.play();
    // Beat 12 at 150 BPM is 4.8 s, frame 288.
    tick(&mut engine, 288);
    assert_eq!(engine.transport_frame(), 288);
    let before = engine.current_beat_f64();
    let old_map = engine.project().unwrap().tempo_map.clone();
    settings_write(&mut engine, |p| {
        p.tempo_map
            .add_or_replace_point(Beats(8.0), Bpm(128.0), TempoPointSource::Manual, 0.001)
    });
    engine.reconcile_tempo_edit(&old_map, Bpm(150.0));
    // Beat 12 now sits at 3.2 s + 4 beats at 128 BPM = 5.075 s, frame 304.5.
    assert_eq!(engine.transport_frame(), 305);
    let per_frame = 128.0 / 60.0 / f64::from(FPS);
    let after = engine.current_beat_f64();
    assert!(
        after >= before && after < before + per_frame,
        "beat {before} became {after}"
    );
    tick(&mut engine, 1);
    assert_eq!(engine.transport_frame(), 306);
    assert!(
        (engine.current_beat_f64() - after - per_frame).abs() < 1e-9,
        "the new tempo holds"
    );
}

/// D10: a project fps change keeps the time, moves the index to the first
/// frame at or after it, then advances on the new grid.
#[test]
fn fps_change_remaps_forward() {
    let mut engine = engine_with(constant_tempo(120.0, 60.0, &[]));
    engine.play();
    for _ in 0..7 {
        tick(&mut engine, 1);
    }
    assert_eq!(engine.transport_frame(), 7);
    settings_write(&mut engine, |p| p.settings.frame_rate = 24.0);
    tick(&mut engine, 1);
    // 7/60 s is frame 2.8 at 24 fps: remapped forward to 3, then one advance.
    assert_eq!(engine.frame_rate(), 24.0);
    assert_eq!(engine.transport_frame(), 4);
    assert_eq!(engine.current_time(), engine.transport_clock().time_at(4));
    assert_eq!(engine.tick_dt(), engine.transport_clock().step());
}

/// Pause keeps the frame and the beat; resume advances from it without
/// repeating it; stop goes to frame 0. Paused ticks still carry their dt (D5).
#[test]
fn stop_resets_pause_keeps() {
    let mut engine = engine_with(constant_tempo(120.0, FPS, &[(0.0, 100.0)]));
    engine.play();
    for _ in 0..100 {
        tick(&mut engine, 1);
    }
    assert_eq!(engine.transport_frame(), 100);
    let beat = engine.current_beat_f64();
    engine.pause();
    tick(&mut engine, 1);
    tick(&mut engine, 3);
    assert_eq!(engine.transport_frame(), 100);
    assert_eq!(engine.current_beat_f64().to_bits(), beat.to_bits());
    assert_eq!(
        engine.tick_dt().0,
        3.0 * engine.transport_clock().step().0,
        "paused graphs keep moving"
    );
    engine.play();
    tick(&mut engine, 1);
    assert_eq!(
        engine.transport_frame(),
        101,
        "resume renders the next frame, not the paused one again"
    );
    engine.stop();
    assert_eq!(engine.transport_frame(), 0);
    assert_eq!(engine.current_time(), Seconds::ZERO);
    assert!(
        (0.0..1e-6).contains(&engine.current_beat_f64()),
        "{}",
        engine.current_beat_f64()
    );
    tick(&mut engine, 2);
    assert_eq!(
        engine.transport_frame(),
        0,
        "stopped ticks don't move the transport"
    );
}

fn first_session_frame(bpm: f32) -> Option<i64> {
    let (project, layer, scene) = session_project(bpm);
    let mut engine = engine_with(project);
    engine.session_set_quantize(Beats(4.0));
    engine.play();
    tick(&mut engine, 10);
    engine.session_launch_slot(layer, scene);
    for _ in 0..200 {
        tick(&mut engine, 1);
        if engine.session_runtime().playing_count() > 0 {
            return Some(engine.transport_frame());
        }
    }
    None
}

/// A launch quantised to beat 4 activates on the first frame whose beat
/// reaches it: exactly on frame 96 at 150 BPM (on the grid, D14), and on
/// frame 113 at 128 BPM, where beat 4 falls at frame 112.5.
#[test]
fn session_launch_on_first_grid_frame() {
    assert_eq!(first_session_frame(150.0), Some(96));
    assert_eq!(first_session_frame(128.0), Some(113));
}

/// The phantom clip a NoteOn creates, stamped at the beat on screen.
fn stamp_note_on(seed: Option<u64>) -> (i64, u64, u64) {
    let Fixture { project, .. } = fixture();
    let mut engine = engine_with(project);
    let anchor = Instant::now();
    let events = schedule(anchor, seed);
    let mut stamp = None;
    drive(&mut engine, anchor, &events, |engine, index| {
        if stamp.is_none() && index >= NOTE_AFTER_INDEX {
            // MIDI drains before the next engine tick, so the note sees this
            // frame's beat. The trigger only reads the engine as its host.
            let mut project = engine.project().expect("project").clone();
            let clip = manifold_playback::live_clip_manager::LiveClipManager::new()
                .trigger_live_content_clip(&mut project, &*engine, 0, 1.0, None, -1, false, 0.0, -1)
                .expect("phantom clip");
            stamp = Some((
                index,
                clip.start_beat.0.to_bits(),
                engine.current_beat_f64().to_bits(),
            ));
        }
    });
    stamp.expect("the note fired")
}

/// NoteOn stamps the previous grid frame's beat, the same bits whatever the
/// press phase or jitter.
#[test]
fn noteon_stamps_previous_grid_beat() {
    let steady = stamp_note_on(None);
    assert_eq!(steady.0, NOTE_AFTER_INDEX);
    assert_eq!(
        steady.1, steady.2,
        "the phantom clip starts at the beat of the frame on screen"
    );
    assert_eq!(stamp_note_on(Some(SEED)), steady, "same frame, same stamp");
}

/// D3: a tick 5 frames late skips the passed frames and stays in time, and a
/// quantised launch whose target the skip jumped over activates on the
/// landing frame.
#[test]
fn stall_skips_and_stays_in_time() {
    let (project, layer, scene) = session_project(150.0);
    let mut engine = engine_with(project);
    engine.session_set_quantize(Beats(4.0));
    let anchor = Instant::now();
    let mut grid = FrameGrid::new(FPS, anchor);
    engine.play();
    for n in 1..=94 {
        let frames = grid.consume_at(grid.deadline(n));
        tick(&mut engine, frames);
    }
    assert_eq!(engine.transport_frame(), 94);
    // The launch targets beat 4, frame 96 at 150 BPM.
    engine.session_launch_slot(layer, scene);
    let step = 1.0 / f64::from(FPS);
    let frames = grid.consume_at(offset(grid.deadline(99), 0.3 * step));
    assert_eq!(frames, 5);
    tick(&mut engine, frames);
    assert_eq!(engine.transport_frame(), 99);
    assert_eq!(engine.current_time(), engine.transport_clock().time_at(99));
    assert_eq!(engine.tick_dt().0, 5.0 * engine.transport_clock().step().0);
    assert!(
        engine.session_runtime().playing_count() > 0,
        "the jumped-over launch activates on the landing frame"
    );
    let frames = grid.consume_at(grid.deadline(100));
    assert_eq!(frames, 1, "the next on-time tick is one frame");
    tick(&mut engine, frames);
    assert_eq!(engine.transport_frame(), 100);
}

/// D4: a seek while playing snaps forward and holds one advance, so the next
/// tick renders the target frame.
#[test]
fn seek_while_playing_renders_target_first() {
    // 150 BPM: the clip at beat 2.5 starts at 1.0 s, frame 60.
    let mut engine = engine_with(constant_tempo(150.0, FPS, &[(2.5, 1.0)]));
    let clip = engine.project().unwrap().timeline.layers[0].clips[0]
        .id
        .clone();
    engine.play();
    tick(&mut engine, 1);
    tick(&mut engine, 1);
    engine.seek_to(Seconds(1.0));
    tick(&mut engine, 1);
    assert_eq!(
        engine.transport_frame(),
        60,
        "the first tick after a playing seek renders the target"
    );
    assert!(
        is_active(&engine, &clip),
        "the clip's first frame is on screen"
    );
    tick(&mut engine, 1);
    assert_eq!(engine.transport_frame(), 61);
    // 1.004 s is frame 60.24: snapped forward to 61.
    engine.seek_to(Seconds(1.004));
    tick(&mut engine, 1);
    assert_eq!(engine.transport_frame(), 61);
}

/// D4: play anchors on the frame already on screen and the first playing
/// tick moves on from it.
#[test]
fn play_advances_from_anchor_without_repeat() {
    let mut engine = engine_with(constant_tempo(120.0, FPS, &[]));
    // 0.81 s is frame 48.6: the stopped transport shows frame 49.
    engine.locate(Seconds(0.81));
    assert_eq!(engine.transport_frame(), 49);
    engine.play();
    assert_eq!(
        engine.transport_frame(),
        49,
        "play anchors on the frame on screen"
    );
    tick(&mut engine, 1);
    assert_eq!(engine.transport_frame(), 50);
    engine.stop();
    engine.play();
    tick(&mut engine, 1);
    assert_eq!(
        engine.transport_frame(),
        1,
        "from stop, frame 0 was on screen"
    );
}

/// Export through the engine's own export sequence, the one `content_export`
/// runs: the restore point, stop, export mode, `begin_export_run` (with one
/// warm-up tick standing in for a video project's decoder wait), frame k's
/// `export_frame_context`, then `end_export_run`.
fn export(
    engine: &mut PlaybackEngine,
    fps: f32,
    start: Seconds,
    n: u32,
    on_frame: impl FnMut(&PlaybackEngine, u32),
) {
    export_with(engine, fps, start, n, true, on_frame);
}

/// `export`, warming up only when `warm_up` is set: production skips the
/// warm-up for a project with no video clips.
fn export_with(
    engine: &mut PlaybackEngine,
    fps: f32,
    start: Seconds,
    n: u32,
    warm_up: bool,
    mut on_frame: impl FnMut(&PlaybackEngine, u32),
) {
    let restore = engine.export_restore_point();
    engine.stop();
    engine.set_export_mode(true);
    engine.begin_export_run(fps, start, |engine| {
        if warm_up {
            engine.export_warm_up_tick();
        }
        warm_up
    });
    for k in 0..n {
        let result = engine.tick(PlaybackEngine::export_frame_context(k, Seconds::ZERO));
        engine.reclaim_tick_result(result);
        on_frame(engine, k);
    }
    engine.end_export_run(restore);
}

/// D9, D17: export frames sit on the song-time-0 grid. At the project rate
/// they match live play on the same frame bit for bit. At half the rate the
/// time is the same bits and the clips the same; the beat is sampled 1e-7 of
/// an export frame late instead of a live one (D14), so beats and params
/// agree to within that.
#[test]
fn export_frames_match_live_indices() {
    let Fixture {
        project,
        session_layer,
        ..
    } = fixture();
    let clip_ids = all_clip_ids(&project);
    let mut live = engine_with(project.clone());
    live.play();
    let mut live_frames = BTreeMap::new();
    for _ in 0..600 {
        tick(&mut live, 1);
        live_frames.insert(
            live.transport_frame(),
            capture(&live, &clip_ids, &session_layer),
        );
    }
    // From 0 s, as production exports this generator-only project (no
    // warm-up), every value matches live but the clip-edge step's param: a
    // step reaches its param one tick after it fires (`evaluate_modulation`
    // writes the step shadows before this tick's starts advance them).
    // Export frame 0 ticks the start frame, which live play never does, so
    // the step of the clip starting at 0 shows on export frame 1 and on live
    // frame 2: at the project rate that param is left out on export frame 1
    // only, and every other value on every frame is exact. At half the rate
    // the lag is one export frame, two live ones, so that param is left out.
    // From 2.01 s, off both grids, export frame 0 is the first frame at or
    // after it, with the warm-up and re-seek a video project runs. There the
    // step envelope is history, not grid: it counts edges since playback
    // started, which live play from 0 and export from 2.01 s do not share,
    // so it alone is left out; `param_step_clip_edge` covers its live and
    // export counts.
    let level = 3;
    for (start, fps, ratio, first, warm_up) in [
        (0.0, 60.0, 1, 0, false),
        (0.0, 30.0, 2, 0, false),
        (2.01, 60.0, 1, 121, true),
        (2.01, 30.0, 2, 61, true),
    ] {
        let mut engine = engine_with(project.clone());
        export_with(
            &mut engine,
            fps,
            Seconds(start),
            200,
            warm_up,
            |engine, k| {
                let index = engine.transport_frame();
                let at = format!("export from {start} s at {fps} fps, frame {k}");
                assert_eq!(index, first + k as i64, "{at}");
                let Some(live) = live_frames.get(&(index * ratio)) else {
                    assert_eq!(index, 0, "live play renders every frame after 0");
                    return;
                };
                let mut frame = capture(engine, &clip_ids, &session_layer);
                let history = start != 0.0;
                if history || ratio != 1 || k == 1 {
                    frame.param_bits[level] = live.param_bits[level];
                }
                if history {
                    frame.step_values.clone_from(&live.step_values);
                }
                if ratio == 1 {
                    let one = BTreeMap::from([(index, frame)]);
                    if let Some(divergence) = first_divergence(&live_frames, &one) {
                        panic!("{at}: {divergence}");
                    }
                    return;
                }
                assert_eq!(frame.time_bits, live.time_bits, "{at}: time");
                assert_eq!(frame.active_clips, live.active_clips, "{at}: clips");
                assert_eq!(frame.session, live.session, "{at}: session");
                let beat_gap =
                    (f64::from_bits(frame.beat_bits) - f64::from_bits(live.beat_bits)).abs();
                assert!(beat_gap < 1e-8, "{at}: beats {beat_gap} apart");
                for (p, (a, b)) in frame.param_bits.iter().zip(&live.param_bits).enumerate() {
                    let gap = (f32::from_bits(*a) - f32::from_bits(*b)).abs();
                    assert!(gap < 1e-5, "{at}: param {p} {gap} apart");
                }
            },
        );
    }
}

fn start_count(engine: &PlaybackEngine, clip: &ClipId) -> usize {
    stub(engine).start_count_for(clip.as_str())
}

fn stub(engine: &PlaybackEngine) -> &StubRenderer {
    engine.renderers()[0]
        .as_any()
        .downcast_ref::<StubRenderer>()
        .expect("stub renderer")
}

/// A 24 fps export of a 60 fps project starting at 1.04 s (beat 2.08 at 120
/// BPM): export frame 0 is index 25 (1.0417 s, beat 2.083). A clip at beat
/// 2.09 (1.045 s) starts after it, so it first shows on export frame 1 and
/// starts exactly once: a seek on the 60 fps grid (1.05 s) would start it
/// before export frame 0.
#[test]
fn export_clip_inside_first_frame() {
    let mut engine = engine_with(constant_tempo(120.0, 60.0, &[(2.09, 4.0)]));
    let clip = engine.project().unwrap().timeline.layers[0].clips[0]
        .id
        .clone();
    let mut shown = Vec::new();
    export_with(&mut engine, 24.0, Seconds(1.04), 3, false, |engine, _| {
        shown.push((
            engine.transport_frame(),
            is_active(engine, &clip),
            start_count(engine, &clip),
        ));
    });
    assert_eq!(shown, vec![(25, false, 0), (26, true, 1), (27, true, 1)]);
    assert_eq!(
        engine.frame_rate(),
        60.0,
        "the end of export returns to the project rate"
    );
}

/// An export returns the transport to the frame it left, at every tempo,
/// playing or paused (D14: the restore point is the frame's exact beat, and
/// a seek snaps wider than the sample offset).
#[test]
fn export_restores_the_frame_it_left() {
    for bpm in [120.0, 128.0, 141.0, 150.0, 174.0] {
        let mut engine = engine_with(constant_tempo(bpm, FPS, &[(0.0, 400.0)]));
        let mut moved = Vec::new();
        for k in (1..3_000).step_by(7).chain([86_399, 216_001]) {
            let time = engine.transport_clock().time_at(k);
            engine.locate(time);
            let state = if k % 2 == 0 {
                PlaybackState::Playing
            } else {
                PlaybackState::Paused
            };
            engine.set_state(state);
            export(&mut engine, 24.0, Seconds(2.0), 2, |_, _| {});
            if engine.transport_frame() != k {
                moved.push((k, engine.transport_frame()));
            }
            assert_eq!(
                engine.current_state(),
                state,
                "{bpm} BPM frame {k}: state restored"
            );
        }
        assert!(
            moved.is_empty(),
            "{bpm} BPM: {} restores moved, first {:?}",
            moved.len(),
            &moved[..moved.len().min(5)]
        );
    }
}

/// D9, D22: an export under an external authority renders map beats on the
/// export grid and advances every frame; the follower comes back after it,
/// and live ticks advance the grid by their frames.
#[test]
fn export_under_external_sync_advances() {
    let mut engine = engine_with(constant_tempo(120.0, 60.0, &[]));
    engine.set_follow_authority(Some(FollowProfile::FAST));
    let mut frames = Vec::new();
    export(&mut engine, 30.0, Seconds(1.0), 5, |engine, _| {
        frames.push((
            engine.transport_frame(),
            engine.follows_external(),
            engine.frame_beat().0,
        ));
    });
    let expected: Vec<_> = (30..35)
        .map(|i| (i, false, i as f64 / 30.0 * 2.0))
        .collect();
    assert_eq!(frames.len(), expected.len());
    for (got, want) in frames.iter().zip(&expected) {
        assert_eq!((got.0, got.1), (want.0, want.1));
        assert!(
            (got.2 - want.2).abs() < 1e-9,
            "export beat {} is the map's {}",
            got.2,
            want.2
        );
    }
    assert!(
        engine.follows_external(),
        "the follower is back after the export"
    );
    assert!(
        engine.follower().awaiting_seed(),
        "export end is a seed point"
    );
    engine.play();
    let parked = engine.transport_frame();
    tick(&mut engine, 1);
    assert_eq!(
        engine.transport_frame(),
        parked + 1,
        "live ticks advance under the follower"
    );
}

/// A sample handed over before a placement in the same tick (AbletonOSC
/// drains a report and then a seek in one loop) never reaches the follower:
/// every placement drops it. Undropped, its error would reach the pre-filter.
#[test]
fn placement_after_an_external_target_wins_the_tick() {
    type Place = fn(&mut PlaybackEngine);
    let places: [(&str, Place, i64); 3] = [
        (
            "seek while playing (held)",
            |e| {
                e.seek_to(Seconds(10.0));
            },
            600,
        ),
        ("locate", |e| e.locate(Seconds(10.0)), 601),
        (
            "stop then play",
            |e| {
                e.stop();
                e.play();
            },
            1,
        ),
    ];
    for (name, place, expected) in places {
        let mut engine = engine_with(constant_tempo(120.0, FPS, &[]));
        engine.set_follow_authority(Some(FollowProfile::FAST));
        engine.play();
        for _ in 0..60 {
            tick(&mut engine, 1);
        }
        engine.set_external_sample(ExternalSample {
            beat: 1000.0,
            bpm: 120.0,
            fresh: true,
            exact: false,
        });
        place(&mut engine);
        tick(&mut engine, 1);
        assert_eq!(engine.transport_frame(), expected, "{name}");
        let map_beat = engine.current_time().0 * 2.0;
        assert!(
            (engine.frame_beat().0 - map_beat).abs() < 1e-9,
            "{name}: beat {} is the map's {map_beat}",
            engine.frame_beat().0
        );
        assert_eq!(
            engine.follower().filtered_error(),
            0.0,
            "{name}: the sample reached the follower"
        );
    }
}

/// D14 at the engine: at 150 BPM / 60 fps, on-frame edges whose frame time
/// computes a hair below their beat still start their clip on that frame.
#[test]
fn on_frame_edges_below_their_beat_start_on_their_frame() {
    let probe = constant_tempo(150.0, FPS, &[]);
    let mut map = probe.tempo_map.clone();
    let step = 1.0 / f64::from(FPS);
    // Every 1/8 beat at 150 BPM is 3 frames: frame 3k is beat k/8.
    let edges: Vec<(i64, f64)> = (1..2_000i64)
        .map(|k| (3 * k, k as f64 / 8.0))
        .filter(|&(frame, beat)| {
            TempoMapConverter::seconds_to_beat_f64(&mut map, frame as f64 * step, Bpm(150.0)) < beat
        })
        .take(12)
        .collect();
    assert!(
        edges.len() >= 8,
        "150 BPM must have on-frame edges that round below: {edges:?}"
    );
    // Each clip 1/16 beat long: shown on its frame and the next.
    let spans: Vec<(f64, f64)> = edges.iter().map(|&(_, beat)| (beat, 1.0 / 16.0)).collect();
    let mut engine = engine_with(constant_tempo(150.0, FPS, &spans));
    let ids: Vec<ClipId> = engine.project().unwrap().timeline.layers[0]
        .clips
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let last = edges.last().unwrap().0 + 3;
    engine.play();
    for frame in 1..=last {
        tick(&mut engine, 1);
        assert_eq!(engine.transport_frame(), frame);
        for ((edge_frame, beat), id) in edges.iter().zip(&ids) {
            let expected = frame == *edge_frame || frame == edge_frame + 1;
            assert_eq!(
                is_active(&engine, id),
                expected,
                "clip at beat {beat} (frame {edge_frame}) on frame {frame}"
            );
        }
    }
}

/// D5: every consumer gets `frames × step` as its dt, playing and paused,
/// never a wall-clock value; `pre_render` is the one renderers see.
#[test]
fn tick_dt_reaches_consumers_as_whole_frames() {
    let mut engine = engine_with(constant_tempo(120.0, FPS, &[(0.0, 8.0)]));
    let step = engine.transport_clock().step().0;
    engine.play();
    for (state, frames) in [
        (PlaybackState::Playing, 1),
        (PlaybackState::Playing, 3),
        (PlaybackState::Paused, 2),
        (PlaybackState::Paused, 1),
    ] {
        engine.set_state(state);
        let result = engine.tick(TickContext {
            frames,
            realtime_now: Seconds(0.123_456 * f64::from(frames)),
            frame_count: 7,
        });
        engine.reclaim_tick_result(result);
        let expected = (f64::from(frames) * step) as f32;
        assert_eq!(
            stub(&engine).last_pre_render_dt().map(f32::to_bits),
            Some(expected.to_bits()),
            "{state:?}, {frames} frames"
        );
        assert_eq!(
            engine.tick_dt().0.to_bits(),
            (f64::from(frames) * step).to_bits()
        );
    }
}

/// Frames between the restarts of an unquantised session loop launched on
/// `launch_after` (one clip spanning the loop, so each wrap restarts it).
fn loop_restart_intervals(bpm: f32, length: f64, launch_after: i64, loops: usize) -> Vec<i64> {
    let mut project = constant_tempo(bpm, FPS, &[]);
    let layer = project.timeline.layers[0].layer_id.clone();
    let scene = SceneId::new("loop-scene");
    project.session.scenes.push(Scene {
        id: scene.clone(),
        name: "Scene".into(),
        color: None,
    });
    project.session.slots.push(SessionSlot {
        layer_id: layer.clone(),
        scene_id: scene.clone(),
        name: "Loop".into(),
        color: None,
        sequence: ClipSequence {
            length_beats: Beats(length),
            clips: clips(&[(0.0, length)]),
        },
    });
    let clip = project.session.slots[0].sequence.clips[0].id.clone();
    let mut engine = engine_with(project);
    engine.session_set_quantize(Beats::ZERO);
    engine.play();
    tick(&mut engine, u32::try_from(launch_after).unwrap());
    engine.session_launch_slot(layer, scene);
    assert_eq!(
        start_count(&engine, &clip),
        1,
        "quantize 0 launches on the frame it is pressed"
    );
    let mut starts = vec![engine.transport_frame()];
    while starts.len() <= loops {
        let before = start_count(&engine, &clip);
        tick(&mut engine, 1);
        if start_count(&engine, &clip) > before {
            starts.push(engine.transport_frame());
        }
    }
    starts.windows(2).map(|w| w[1] - w[0]).collect()
}

/// An unquantised loop is anchored at the frame's exact beat, so it restarts
/// every whole loop of frames: 60 for 2 beats at 120 BPM, 12 for ½ beat at
/// 150 BPM, over 100 loops from several launch frames. Anchored at the
/// sampled beat it drifts between 59 and 61.
#[test]
fn unquantised_loops_restart_on_whole_frames() {
    for (bpm, length, frames) in [(120.0, 2.0, 60), (150.0, 0.5, 12)] {
        for launch_after in [37, 41, 53, 64, 97, 128] {
            let intervals = loop_restart_intervals(bpm, length, launch_after, 100);
            let off: Vec<(usize, i64)> = intervals
                .iter()
                .copied()
                .enumerate()
                .filter(|&(_, d)| d != frames)
                .collect();
            assert!(
                off.is_empty(),
                "{bpm} BPM {length}-beat loop from frame {launch_after}: {} of 100 restarts off {frames} frames, first {:?}",
                off.len(),
                &off[..off.len().min(5)]
            );
        }
    }
}

/// A user settings write through the gateway, as the content thread makes it.
fn settings_write(
    engine: &mut PlaybackEngine,
    f: impl FnOnce(&mut manifold_core::project::Project),
) {
    let settings = manifold_core::change::ChangeSet::of(&[manifold_core::change::Part::Settings]);
    let edit = engine.project_edit().expect("project");
    manifold_editing::service::EditingService::new().mutate(settings, edit, f);
}

/// Export plays its own clock from a zero tempo shift: an export, a BPM edit
/// and back while playing, then the same export again give every frame the
/// same physics clock (played time, placement target and replay origin), so
/// the two exports render the same physics.
#[test]
fn an_export_after_a_bpm_edit_and_back_matches_the_export_before_it() {
    fn physics_clock(engine: &mut PlaybackEngine) -> Vec<[u64; 5]> {
        let mut frames = Vec::new();
        export(engine, FPS, Seconds(2.01), 120, |engine, _| {
            frames.push([
                engine.current_time_double().to_bits(),
                engine.played_time().seconds().to_bits(),
                engine
                    .played_target()
                    .map_or(u64::MAX, |target| target.seconds().to_bits()),
                engine.replay_origin().seconds().to_bits(),
                engine.tempo_shift().0.to_bits(),
            ]);
        });
        frames
    }
    fn set_bpm(engine: &mut PlaybackEngine, bpm: f32) {
        let old_map = engine.project().unwrap().tempo_map.clone();
        let old_bpm = engine.project().unwrap().settings.bpm;
        settings_write(engine, |project| {
            project.settings.bpm = Bpm(bpm);
            project.tempo_map.add_or_replace_point(
                Beats::ZERO,
                Bpm(bpm),
                TempoPointSource::Manual,
                0.001,
            );
        });
        engine.reconcile_tempo_edit(&old_map, old_bpm);
    }
    let mut engine = engine_with(constant_tempo(128.0, FPS, &[]));
    let before = physics_clock(&mut engine);
    engine.play();
    tick(&mut engine, 30);
    set_bpm(&mut engine, 150.0);
    tick(&mut engine, 10);
    set_bpm(&mut engine, 128.0);
    tick(&mut engine, 10);
    assert_ne!(
        engine.tempo_shift(),
        Seconds::ZERO,
        "the edits moved the transport"
    );
    let after = physics_clock(&mut engine);
    assert_eq!(before.len(), 120);
    assert!(
        before == after,
        "the export after the edits differs from the one before them"
    );
}
