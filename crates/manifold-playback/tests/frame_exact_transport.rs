//! Frame-exact transport, engine level (docs/FRAME_EXACT_TRANSPORT_DESIGN.md
//! section 5, BUG-gqhep (live-transport-nondeterministic-frame-clock)).
//!
//! The same arrangement played twice must give the same values on the same
//! transport frame, whatever the Play-press phase, tick jitter or stalls.
//! Committed red: today the engine adds measured wall time, so a jittered run
//! samples different beats. Only `drive` changes when P2 wires the clock; P2
//! deletes the `frame-exact-red` feature in the commit that turns this green.
#![cfg(feature = "frame-exact-red")]

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
use manifold_core::types::{BeatDivision, DriverWaveform, TempoPointSource};
use manifold_core::{Beats, Bpm, ClipId, LayerId, PresetTypeId, SceneId, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
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
    }));
}

fn point(beat: f64, value: f32, shape: SegmentShape) -> AutomationPoint {
    AutomationPoint { beat: Beats(beat), value, shape }
}

fn clips(spans: &[(f64, f64)]) -> Vec<TimelineClip> {
    spans.iter().map(|&(start, length)| TimelineClip::new_generator(Beats(start), Beats(length))).collect()
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
        project.tempo_map.add_or_replace_point(Beats(beat), Bpm(bpm), TempoPointSource::Manual, 0.001);
    }

    let mut main = Layer::new_generator("Main".into(), PresetTypeId::new("TestGen"), 0);
    let gp = main.gen_params_or_init();
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
    envelope.action = TriggerAction::Step { amount: 1.0, wrap: WrapMode::Wrap };
    gp.envelopes = Some(vec![envelope]);
    // On-frame starts and ends every half beat at 150 BPM, then off-grid starts.
    let mut spans: Vec<(f64, f64)> = (0..16).map(|i| (i as f64 * 0.5, 0.375)).collect();
    spans.extend([8.3, 9.17, 11.05, 13.333, 17.71, 21.9, 26.41, 30.07, 33.13, 35.55].map(|s| (s, 0.6)));
    main.clips = clips(&spans);

    // Tempo boundaries (8, 16, 32) and sub-beat starts as clip edges.
    let mut second = Layer::new_generator("Second".into(), PresetTypeId::new("TestGen"), 1);
    second.clips = clips(&[(0.125, 0.25), (3.0625, 0.5), (8.0, 1.0), (16.0, 2.0), (32.0, 1.0)]);

    let session = Layer::new_generator("Session".into(), PresetTypeId::new("TestGen"), 2);
    let session_layer = session.layer_id.clone();
    let scene = SceneId::new("frame-exact-scene");
    project.session.scenes.push(Scene { id: scene.clone(), name: "Scene".into(), color: None });
    project.session.slots.push(SessionSlot {
        layer_id: session_layer.clone(),
        scene_id: scene.clone(),
        name: "Loop".into(),
        color: None,
        sequence: ClipSequence { length_beats: Beats(2.0), clips: clips(&[(0.0, 0.5), (1.0, 0.25)]) },
    });

    project.timeline.layers = vec![main, second, session];
    Fixture { project, session_layer, scene }
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
    let session = project.session.slots.iter().flat_map(|s| s.sequence.clips.iter());
    timeline.chain(session).map(|c| c.id.clone()).collect()
}

fn capture(engine: &PlaybackEngine, clip_ids: &[ClipId], session_layer: &LayerId) -> Frame {
    let project = engine.project().expect("project");
    let renderer = &engine.renderers()[0];
    let instances = project.timeline.layers.iter().filter_map(|l| l.gen_params());
    let mut param_bits = Vec::new();
    let mut step_values = Vec::new();
    for gp in instances {
        param_bits.extend(gp.params.iter().map(|p| p.value.to_bits()));
        step_values.extend(gp.envelopes.iter().flatten().map(|e| e.step_value.map(f32::to_bits)));
    }
    let runtime = engine.session_runtime();
    Frame {
        time_bits: engine.current_time_double().to_bits(),
        beat_bits: engine.current_beat_f64().to_bits(),
        active_clips: (0..clip_ids.len()).filter(|&i| renderer.is_active(clip_ids[i].as_str())).collect(),
        param_bits,
        step_values,
        session: (
            runtime.playing_scene(session_layer).map(|s| format!("{s:?}")),
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
    if seconds >= 0.0 { base + Duration::from_secs_f64(seconds) } else { base - Duration::from_secs_f64(-seconds) }
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
        deadline += if seed.is_some() && tick == STALL_AT_TICK { 4 } else { 1 };
        events.push(Event::Tick(offset(grid.deadline(deadline), jitter())));
    }
    // Commands drain at the start of a tick, so a press orders by its instant.
    events.sort_by_key(Event::at);
    events
}

/// Feed one run's events to the engine; `on_frame` sees each playing tick
/// with the transport frame index it rendered. The one part of this file P2
/// changes: today each tick hands the engine the wall time since the previous
/// tick, as `FrameTimer::consume_tick_at` hands the content thread, and the
/// index is the D2 grid's frame count since Play. After P2 the tick hands the
/// engine those frames and the index comes from the engine's clock.
fn drive(engine: &mut PlaybackEngine, anchor: Instant, events: &[Event], mut on_frame: impl FnMut(&mut PlaybackEngine, i64)) {
    let mut grid = FrameGrid::new(FPS, anchor);
    let mut last_tick = anchor;
    let mut index = 0i64;
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
        let dt = Seconds(now.saturating_duration_since(last_tick).as_secs_f64());
        last_tick = now;
        let playing = engine.is_playing();
        let result = engine.tick(TickContext {
            dt_seconds: dt,
            realtime_now: Seconds(now.saturating_duration_since(anchor).as_secs_f64()),
            pre_render_dt: dt,
            frame_count,
            export_fixed_dt: Seconds::ZERO,
        });
        engine.reclaim_tick_result(result);
        frame_count += 1;
        if playing {
            index += i64::from(frames);
            on_frame(engine, index);
        }
    }
}

fn run(seed: Option<u64>) -> BTreeMap<i64, Frame> {
    let Fixture { project, session_layer, scene } = fixture();
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
    let (index, fa, fb) = a.iter().filter_map(|(i, fa)| b.get(i).map(|fb| (i, fa, fb))).find(|(_, fa, fb)| fa != fb)?;
    let field = if fa.time_bits != fb.time_bits {
        format!("time {} vs {}", seconds(fa.time_bits), seconds(fb.time_bits))
    } else if fa.beat_bits != fb.beat_bits {
        format!("beat {} vs {}", seconds(fa.beat_bits), seconds(fb.beat_bits))
    } else if fa.active_clips != fb.active_clips {
        format!("active clips {:?} vs {:?}", fa.active_clips, fb.active_clips)
    } else if fa.param_bits != fb.param_bits {
        let p = fa.param_bits.iter().zip(&fb.param_bits).position(|(x, y)| x != y).unwrap_or(0);
        format!("param {p}: {} vs {}", f32::from_bits(fa.param_bits[p]), f32::from_bits(fb.param_bits[p]))
    } else if fa.step_values != fb.step_values {
        format!("envelope steps {:?} vs {:?}", fa.step_values, fb.step_values)
    } else {
        format!("session {:?} vs {:?}", fa.session, fb.session)
    };
    let shared = a.keys().filter(|i| b.contains_key(i)).count();
    let differing = a.iter().filter(|(i, fa)| b.get(i).is_some_and(|fb| fb != *fa)).count();
    Some(format!("{differing} of {shared} shared transport frames differ; the first is frame {index}: {field}"))
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
    assert!(f64::from_bits(last.beat_bits) > 33.0, "the run must cross the 141 BPM change at beat 32");
    let param = |p: usize| distinct(frames.values().map(move |f| f.param_bits[p]));
    assert!(param(0) >= 8, "automation must move: {} values", param(0));
    assert!(param(1) >= 100 && param(2) >= 100, "both LFOs must move: {} and {}", param(1), param(2));
    let steps = distinct(frames.values().map(|f| f.step_values[0]));
    assert!(steps >= 8, "the clip-edge step envelope must advance: {steps} values");
    let memberships = distinct(frames.values().map(|f| f.active_clips.len()));
    assert!(memberships >= 3, "clips must start and stop: {memberships} membership sizes");
    let launched = frames.iter().find(|(_, f)| f.session.2 > 0).map(|(i, _)| *i);
    assert!(launched.is_some_and(|i| i > LAUNCH_AFTER_INDEX), "the quantised launch must wait for its bar: {launched:?}");
}

#[test]
fn same_values_per_index_across_phase_jitter_and_stalls() {
    let steady = run(None);
    assert_eq!(steady.len(), PLAYING_TICKS, "one frame per steady tick");
    assert_not_vacuous(&steady);
    // The harness itself is repeatable: identical instants give identical frames.
    assert_eq!(first_divergence(&steady, &run(None)), None, "the steady run must repeat bit for bit");

    let jittered = run(Some(SEED));
    let shared = steady.keys().filter(|i| jittered.contains_key(i)).count();
    assert!(shared + 3 >= PLAYING_TICKS, "the jittered run must render all but the stalled frames: {shared}");
    if let Some(divergence) = first_divergence(&steady, &jittered) {
        panic!("same arrangement, different values per transport frame: {divergence}");
    }
}
