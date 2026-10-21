//! Speed reaches a simulation clock at each tick's start, whatever the
//! display rate, live or exported: from a host param ramped by an arrangement
//! automation lane (BUG-mjm2o (host-automated Speed held across the replay)),
//! from a lane plus an audio mod on the same param, from Hold steps and
//! in-graph steps that land exactly on tick starts, and after a seek back to
//! a time between frames. Drives real playback automation and modulation into
//! a generator whose Speed is a host param or an in-graph curve.
use manifold_core::audio_features::{AudioFeatureHop, AudioFeatureSnapshot, AudioHopBatch, AudioHopStamp, SendFeatures};
use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
use manifold_core::audio_setup::AudioSend;
use manifold_core::audio_trigger::FireMeterCapture;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};
use manifold_core::layer::Layer;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::project::Project;
use manifold_core::{tempo::TempoMap, units::Bpm, Beats, PresetTypeId, Seconds};
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime, SimStep};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::runtime::preset_context::ProjectTempo;
use manifold_node_engine::runtime::*;
use manifold_physics::clock::{SimulationClock, TICK};
use manifold_playback::automation::{AutomationGestures, AutomationLatches, AutomationSamplingMode};
use std::{borrow::Cow, cell::RefCell};

thread_local! {
    /// Every tick the clock ran, as its simulated interval's bits.
    static TICKS: RefCell<Vec<(u64, u64)>> = const { RefCell::new(Vec::new()) };
}

const LIQUID: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
const SAMPLE_RATE: u32 = 48_000;
const HOP: u64 = 480;
/// A tempo whose beats are inexact on the 60 Hz tick grid.
const BPM: f64 = 128.0;
/// Speed of each three-tick step: tick `j` of a 60 Hz grid runs at
/// `STEPS[(j / 3) % 4]`.
const STEPS: [f32; 4] = [0.5, 1.0, 1.5, 2.0];

/// Stands in for a GPU liquid's clock: observes its Speed in each replay
/// sample, as the domains do, and runs the frame's ticks.
struct TickedClock(EffectNodeType, SimulationClock, [NodeOutput; 1]);

impl EffectNode for TickedClock {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("speed"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Input,
            required: false,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.2
    }
    fn parameters(&self) -> &[ParamDef] {
        static PARAMS: [ParamDef; 1] = [ParamDef {
            name: Cow::Borrowed("speed"),
            label: "Speed",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 4.0)),
            enum_values: &[],
        }];
        &PARAMS
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let now = ctx.time.seconds.0;
        let speed = ctx.scalar_or_param("speed", 1.0);
        self.1.set_transport_target(ctx.sim_step.transport_target.map(|target| target.0));
        if ctx.sim_step.authored_sample_only {
            self.1.observe_speed(now, speed);
            return;
        }
        let frame = self.1.advance(now, ctx.sim_step.interval.0, speed, 0.0, false, ctx.sim_step.offline());
        TICKS.with_borrow_mut(|ticks| {
            ticks.extend((0..u64::from(frame.ticks)).map(|tick| {
                let interval = frame.interval(tick).unwrap();
                (interval.start.0.to_bits(), interval.end.0.to_bits())
            }));
        });
    }
}

impl manifold_nodes_physics::tick_sampled::TickSampled for TickedClock {
    fn request_physics_samples(&mut self, from: f64, until: f64, out: &mut Vec<f64>) {
        self.1.tick_starts(from, until, |transport| out.push(transport));
    }
}

inventory::submit! {
    manifold_nodes_physics::tick_sampled::TickSampledRegistration::new::<TickedClock>()
}

/// An in-graph curve on the beat: steps through [`STEPS`] at the beat of
/// every third 60 Hz tick start, an edge exactly on a tick start.
struct BeatSteps(EffectNodeType, [NodeOutput; 1]);

impl EffectNode for BeatSteps {
    fn is_pure(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.1
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let beat = ctx.time.beats.0;
        let reached = (1..64).filter(|&k| step_beat(k) <= beat).count();
        ctx.outputs.set_scalar("out", ParamValue::Float(STEPS[reached % STEPS.len()]));
    }
}

/// The beat of tick `3k` of a 60 Hz grid at [`BPM`], as authored.
fn step_beat(k: usize) -> f64 {
    (8 * k) as f64 / 75.0
}

#[derive(Clone, Copy, PartialEq)]
enum Case {
    /// A linear lane on the host Speed param.
    Lane,
    /// The same lane, plus an audio mod on the same param.
    LaneWithAudio,
    /// A lane of Hold steps, one exactly on every third tick start.
    HoldSteps,
    /// No lane: an in-graph curve steps Speed on every third tick start.
    InGraphSteps,
}

fn runtime(case: Case) -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(LIQUID, || {
        let cell_size = NodePort { name: Cow::Borrowed("cell_size"), ty: PortType::Scalar(ScalarType::F32), kind: PortKind::Output, required: false };
        Box::new(TickedClock(EffectNodeType::new(LIQUID), SimulationClock::default(), [cell_size]))
    });
    registry.register("test.beat_steps", || {
        let out = NodePort { name: Cow::Borrowed("out"), ty: PortType::Scalar(ScalarType::F32), kind: PortKind::Output, required: false };
        Box::new(BeatSteps(EffectNodeType::new("test.beat_steps"), [out]))
    });
    let mut nodes = vec![
        serde_json::json!({"id": 0, "nodeId": "clock", "typeId": LIQUID}),
        serde_json::json!({"id": 3, "nodeId": "source", "typeId": "system.source"}),
        serde_json::json!({"id": 4, "nodeId": "output", "typeId": "system.final_output"}),
        serde_json::json!({"id": 5, "nodeId": "input", "typeId": "system.generator_input"}),
    ];
    let mut wires = vec![serde_json::json!({"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"})];
    if case == Case::InGraphSteps {
        nodes.push(serde_json::json!({"id": 6, "nodeId": "steps", "typeId": "test.beat_steps"}));
        wires.push(serde_json::json!({"fromNode": 6, "fromPort": "out", "toNode": 0, "toPort": "speed"}));
    }
    let def = serde_json::json!({
        "version": 2, "name": "Automated speed",
        "presetMetadata": {
            "id": "AutomatedSpeed", "displayName": "Automated speed", "category": "Test",
            "oscPrefix": "automated_speed", "available": true,
            "params": [{"id": "speed", "name": "Speed", "min": 0.0, "max": 4.0, "defaultValue": 1.0}],
            "bindings": [{"id": "speed", "label": "Speed", "defaultValue": 1.0,
                "target": {"kind": "node", "nodeId": "clock", "param": "speed"}}]
        },
        "nodes": nodes,
        "wires": wires
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

/// The host project. Speed's linear lane is 0.5 at beat 0 rising to 2.0 at
/// beat 4 (1.875 s at 128 BPM), so Speed is 0.5 + 0.8 t.
fn project(case: Case) -> Project {
    let mut project = Project::default();
    let send = AudioSend::new("Kick");
    let send_id = send.id.clone();
    project.audio_setup.sends.push(send);
    let mut layer = Layer::new_generator("Liquid".into(), PresetTypeId::new("AutomatedSpeed"), 0);
    let generator = layer.gen_params_mut().unwrap();
    let mut speed = Param::bundled(ParamSpecDef {
        id: "speed".into(),
        name: "Speed".into(),
        min: 0.0,
        max: 4.0,
        default_value: 1.0,
        ..Default::default()
    });
    speed.value = 1.0;
    speed.base = 1.0;
    generator.params = ParamManifest::from_params(vec![speed]);
    let point = |beat, value, shape| AutomationPoint { beat: Beats(beat), value, shape };
    let points = match case {
        Case::Lane | Case::LaneWithAudio => vec![point(0.0, 0.5, SegmentShape::Linear), point(4.0, 2.0, SegmentShape::Linear)],
        Case::HoldSteps => (0..=20).map(|k| point(step_beat(k), STEPS[k % STEPS.len()], SegmentShape::Hold)).collect(),
        Case::InGraphSteps => Vec::new(),
    };
    if !points.is_empty() {
        generator.automation_lanes = Some(vec![AutomationLane { param_id: "speed".to_owned().into(), enabled: true, points }]);
    }
    if case == Case::LaneWithAudio {
        let mut follow = ParameterAudioMod::new("speed".to_owned().into(), send_id, AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low));
        follow.shape.attack_ms = 20.0;
        follow.shape.release_ms = 60.0;
        generator.audio_mods_mut().push(follow);
    }
    project.timeline.layers = vec![layer];
    project
}

/// A low band that never rests, on the 10 ms hop grid.
fn level(end_sample: u64) -> f32 {
    (0.5 + 0.45 * (end_sample as f64 / 3_000.0).sin()) as f32
}

/// Hops stamped with their transport time, as a producer with a transport
/// mapping stamps them live and in export. Physics reads each hop at that
/// time; live input placed by the wall clock lands wherever the audio clock
/// puts it, which no frame rate decides.
fn hops(range: std::ops::Range<u64>) -> AudioFeatureSnapshot {
    let mut batch = AudioHopBatch::with_capacity(64);
    batch.begin(1);
    for hop in range {
        let end_sample = hop * HOP;
        let mut features = SendFeatures::default();
        features.bands[AudioBand::Low.index()].amplitude = level(end_sample);
        batch
            .push(AudioFeatureHop {
                stamp: AudioHopStamp {
                    epoch: 1,
                    end_sample,
                    sample_rate: SAMPLE_RATE,
                    source_time: None,
                    timeline_time: Some(Seconds(end_sample as f64 / f64::from(SAMPLE_RATE))),
                },
                dt: Seconds(HOP as f64 / f64::from(SAMPLE_RATE)),
                features,
            })
            .unwrap();
    }
    AudioFeatureSnapshot { sends: vec![SendFeatures::default()], hop_batches: vec![batch], ..Default::default() }
}

/// One second at `fps`, as the host plays it: automation, then modulation
/// (with every audio hop that ended by the frame), then the frame. The
/// frame's beat carries the engine's sampling offset.
fn run(case: Case, fps: u32, offline: bool) -> Vec<(u64, u64)> {
    let frames = (0..=fps).map(|frame| (f64::from(frame) / f64::from(fps), None));
    play(case, fps, offline, frames)
}

/// Plays `frames` (each a transport time and, on the frame a seek landed
/// on, the seek's target) and returns every tick run.
fn play(case: Case, fps: u32, offline: bool, frames: impl Iterator<Item = (f64, Option<f64>)>) -> Vec<(u64, u64)> {
    TICKS.with_borrow_mut(Vec::clear);
    let mut runtime = runtime(case);
    let mut project = project(case);
    let tempo = ProjectTempo::new(&TempoMap::default(), Bpm(BPM as f32));
    let (mut latches, mut gestures) = (AutomationLatches::default(), AutomationGestures::default());
    let mode = if offline { AutomationSamplingMode::Export } else { AutomationSamplingMode::Playback };
    let mut next_hop = 1;
    let mut previous: Option<f64> = None;
    for (frame, (seconds, target)) in frames.enumerate() {
        let beat = Beats((seconds + 1e-7 / f64::from(fps)) * BPM / 60.0);
        manifold_playback::automation::evaluate_all_automation_with_mode(&mut project, beat, &mut latches, false, &mut gestures, mode);
        let delivered = (seconds * f64::from(SAMPLE_RATE) / HOP as f64 + 1e-9).floor() as u64 + 1;
        let audio = if case == Case::LaneWithAudio { hops(next_hop..delivered.max(next_hop)) } else { AudioFeatureSnapshot::default() };
        next_hop = delivered.max(next_hop);
        manifold_playback::modulation::evaluate_modulation(
            &mut project,
            beat,
            Seconds(seconds),
            Seconds(1.0 / f64::from(fps)),
            &audio,
            &manifold_playback::clip_controls::ClipControlFrame::default(),
            &mut Vec::new(),
            &mut FireMeterCapture::default(),
        );
        let generator = project.timeline.layers[0].gen_params().unwrap();
        runtime.set_project_tempo(Some(&tempo));
        runtime.set_source_instance(Some(generator));
        runtime.apply_param_values(&generator.params);
        if target.is_some() {
            TICKS.with_borrow_mut(Vec::clear);
        }
        let step = if offline { SimStep::export(Seconds(TICK)) } else { SimStep::live(Seconds(TICK)) };
        runtime.set_sim_step(step.with_transport_target(target.map(Seconds)));
        runtime.execute_frame(FrameTime {
            seconds: Seconds(seconds),
            beats: beat,
            delta: Seconds(previous.map_or(0.0, |previous| (seconds - previous).max(0.0))),
            frame_count: frame as i64,
        });
        previous = Some(seconds);
    }
    TICKS.with_borrow_mut(std::mem::take)
}

fn length(&(start, end): &(u64, u64)) -> f64 {
    f64::from_bits(end) - f64::from_bits(start)
}

/// Every rate, live and exported, runs bit-identical ticks to 60 fps export.
fn assert_same_at_every_rate(case: Case, reference: &[(u64, u64)]) {
    for fps in [30, 60, 120, 144] {
        for offline in [false, true] {
            assert!(run(case, fps, offline) == reference, "{fps} fps, offline {offline}: tick lengths differ");
        }
    }
}

#[test]
fn automated_speed_ticks_the_same_at_30_60_120_144_fps_live_and_export() {
    let reference = run(Case::Lane, 60, true);
    assert_eq!(reference.len(), 60, "one second of 60 Hz ticks");
    for (k, tick) in reference.iter().enumerate() {
        let expected = TICK * (0.5 + 0.8 * k as f64 * TICK);
        assert!((length(tick) - expected).abs() < 1e-7, "tick {k} runs at the lane's Speed at its start: {} against {expected}", length(tick));
    }
    assert_same_at_every_rate(Case::Lane, &reference);
}

/// A lane and an audio mod on one Speed: every tick hears the audio, and the
/// ticks match at every rate.
#[test]
fn lane_and_audio_mod_speed_ticks_the_same_at_30_60_120_144_fps_live_and_export() {
    let reference = run(Case::LaneWithAudio, 60, true);
    let lane = run(Case::Lane, 60, true);
    assert_eq!(reference.len(), lane.len());
    let heard = reference.iter().zip(&lane).filter(|(audio, lane)| length(audio) != length(lane)).count();
    assert!(heard > 50, "the audio mod must reach the ticks, heard on {heard} of {}", lane.len());
    assert_same_at_every_rate(Case::LaneWithAudio, &reference);
}

/// A step exactly on a tick start takes effect at that tick, whether a Hold
/// lane or an in-graph curve makes it, at every rate.
#[test]
fn speed_steps_on_tick_starts_land_on_that_tick_at_30_60_120_144_fps_live_and_export() {
    for case in [Case::InGraphSteps, Case::HoldSteps] {
        let reference = run(case, 60, true);
        assert_eq!(reference.len(), 60, "one second of 60 Hz ticks");
        for (j, tick) in reference.iter().enumerate() {
            let expected = TICK * f64::from(STEPS[(j / 3) % STEPS.len()]);
            assert!((length(tick) - expected).abs() < 1e-12, "tick {j} runs at the step at its start: {} against {expected}", length(tick));
        }
        for fps in [30, 60, 120, 144] {
            for offline in [false, true] {
                let ticks = run(case, fps, offline);
                for (j, tick) in ticks.iter().enumerate() {
                    let expected = TICK * f64::from(STEPS[(j / 3) % STEPS.len()]);
                    assert!((length(tick) - expected).abs() < 1e-12, "{fps} fps, offline {offline}: tick {j} runs at {} against {expected}", length(tick));
                }
                assert!(ticks == reference, "{fps} fps, offline {offline}: tick lengths differ");
            }
        }
    }
}

/// Plays 0.6 s, seeks back to a time on no frame grid, and plays a second
/// more. Returns the ticks run after the seek.
fn after_seek(fps: u32, offline: bool) -> Vec<(u64, u64)> {
    const TARGET: f64 = 0.2137;
    let fps_f = f64::from(fps);
    let before = (0..=(0.6 * fps_f).round() as u32).map(|frame| (f64::from(frame) / fps_f, None));
    // The transport lands on the first frame at or after the target.
    let landed = (TARGET * fps_f - 2e-7).ceil() as u32;
    let after = (landed..=((TARGET + 1.0) * fps_f).floor() as u32)
        .map(move |frame| (f64::from(frame) / fps_f, (frame == landed).then_some(TARGET)));
    play(Case::Lane, fps, offline, before.chain(after))
}

/// After a seek, ticks start on the grid from the seek's target and take
/// Speed there, so they match bit for bit at every rate.
#[test]
fn ticks_after_a_seek_match_at_30_60_120_144_fps_live_and_export() {
    let reference = after_seek(60, true);
    assert!(reference.len() >= 59, "a second of ticks after the seek: {}", reference.len());
    let first = f64::from_bits(reference[0].0);
    assert_eq!(first, 0.0, "the restart starts simulated time");
    for fps in [30, 60, 120, 144] {
        for offline in [false, true] {
            let ticks = after_seek(fps, offline);
            let shared = ticks.len().min(reference.len());
            assert!(shared >= 59, "{fps} fps, offline {offline}: {} ticks after the seek", ticks.len());
            assert!(ticks[..shared] == reference[..shared], "{fps} fps, offline {offline}: ticks after the seek differ");
        }
    }
}

/// Plays half a second, seeks forward to a time on no frame grid, and plays
/// a second more. Returns the ticks run from the seek's frame on.
fn after_forward_seek(fps: u32, offline: bool) -> Vec<(u64, u64)> {
    const TARGET: f64 = 0.9137;
    let fps_f = f64::from(fps);
    let before = (0..=(0.5 * fps_f).round() as u32).map(|frame| (f64::from(frame) / fps_f, None));
    let landed = (TARGET * fps_f - 2e-7).ceil() as u32;
    let after = (landed..=((TARGET + 1.0) * fps_f).floor() as u32)
        .map(move |frame| (f64::from(frame) / fps_f, (frame == landed).then_some(TARGET)));
    play(Case::Lane, fps, offline, before.chain(after))
}

/// A forward seek keeps the simulation and starts the tick grid at the
/// seek's target, so the ticks after it match bit for bit at every rate.
#[test]
fn ticks_after_a_forward_seek_match_at_30_60_120_144_fps_live_and_export() {
    let reference = after_forward_seek(60, true);
    assert!(reference.len() >= 59, "a second of ticks after the seek: {}", reference.len());
    assert!(f64::from_bits(reference[0].0) > 0.0, "the seek keeps the simulated time");
    for fps in [30, 60, 120, 144] {
        for offline in [false, true] {
            let ticks = after_forward_seek(fps, offline);
            let shared = ticks.len().min(reference.len());
            assert!(shared >= 59, "{fps} fps, offline {offline}: {} ticks after the seek", ticks.len());
            assert!(ticks[..shared] == reference[..shared], "{fps} fps, offline {offline}: ticks after the forward seek differ");
        }
    }
}
