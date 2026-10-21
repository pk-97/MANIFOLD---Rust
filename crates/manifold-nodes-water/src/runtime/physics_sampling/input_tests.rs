use crate::runtime::WaterRuntimeExt;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::FrameContextInputs;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_core::Beats;
use manifold_core::NodeId;
use manifold_core::audio_mod::HopValue;
use manifold_core::Seconds;
use manifold_node_engine::exec::effect_node::FrameTime;
use super::*;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, parameters::ParamDef};
use manifold_core::{tempo::TempoMap, types::TempoPointSource, units::Bpm};
use std::{borrow::Cow, cell::RefCell};

#[derive(Debug)]
struct Observation {
    time: FrameTime,
    values: [f32; 5],
    draining: bool,
    authored_only: bool,
}

thread_local! {
    static OBSERVATIONS: RefCell<Vec<Observation>> = const { RefCell::new(Vec::new()) };
}

struct ObservedPhysics(EffectNodeType);

impl EffectNode for ObservedPhysics {
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
        const fn input(name: &'static str) -> NodeInput {
            NodePort {
                name: Cow::Borrowed(name),
                ty: PortType::Scalar(ScalarType::F32),
                kind: PortKind::Input,
                required: true,
            }
        }
        static INPUTS: [NodeInput; 5] = [
            input("value"),
            input("clock"),
            input("beat"),
            input("trigger"),
            input("lfo"),
        ];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let values = ["value", "clock", "beat", "trigger", "lfo"].map(|name| {
            match ctx.inputs.scalar(name) {
                Some(ParamValue::Float(value)) => value,
                value => panic!("missing observed scalar {name}: {value:?}"),
            }
        });
        OBSERVATIONS.with_borrow_mut(|observations| {
            observations.push(Observation {
                time: ctx.time,
                values,
                draining: ctx.sim_step.history_drain_active(),
                authored_only: ctx.sim_step.authored_sample_only,
            })
        });
    }
}

fn runtime() -> PresetRuntime {
    OBSERVATIONS.with_borrow_mut(Vec::clear);
    let mut registry = PrimitiveRegistry::with_builtin();
    manifold_water_rigid::testkit::physics_fixtures::register(&mut registry);
    registry.register("node.physics_world", || {
        Box::new(ObservedPhysics(EffectNodeType::new("node.physics_world")))
    });
    let def = serde_json::json!({
        "version": 2, "name": "Observed physics controls",
        "nodes": [
            {"id": 0, "nodeId": "input", "typeId": "system.generator_input"},
            {"id": 1, "nodeId": "value", "typeId": "node.value"},
            {"id": 2, "nodeId": "lfo", "typeId": "test.physics_wave"},
            {"id": 3, "nodeId": "physics", "typeId": "node.physics_world"},
            {"id": 4, "nodeId": "source", "typeId": "system.source"},
            {"id": 5, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "time", "toNode": 2, "toPort": "clock"},
            {"fromNode": 0, "fromPort": "time", "toNode": 3, "toPort": "clock"},
            {"fromNode": 0, "fromPort": "beat", "toNode": 3, "toPort": "beat"},
            {"fromNode": 0, "fromPort": "trigger_count", "toNode": 3, "toPort": "trigger"},
            {"fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "value"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "lfo"},
            {"fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in"}
        ]
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

fn frame(runtime: &mut PresetRuntime, seconds: f64, value: f32, triggers: f32) -> Vec<Observation> {
    let time = FrameTime {
        seconds: Seconds(seconds),
        beats: runtime
            .water_ref().water.project_tempo
            .as_ref()
            .map_or(Beats(seconds * 2.0), |tempo| {
                TempoMapConverter::seconds_to_beat_immut(
                    tempo.map(),
                    Seconds(seconds),
                    tempo.fallback_bpm(),
                )
            }),
        delta: Seconds(
            runtime
                .water_ref().water.last_frame_time
                .map_or(0.0, |previous| seconds - previous.played.seconds()),
        ),
        frame_count: 0,
    };
    runtime.set_frame_context(FrameContextInputs {
        time: seconds as f32,
        beat: time.beats.0 as f32,
        aspect: 1.0,
        trigger_count: triggers,
        anim_progress: 0.0,
        output_width: 16.0,
        output_height: 16.0,
    });
    let node = runtime
        .graph
        .instance_by_node_id(&NodeId::new("value"))
        .unwrap();
    runtime
        .graph
        .set_param(node, "value", ParamValue::Float(value))
        .unwrap();
    runtime.execute_frame(time);
    OBSERVATIONS.with_borrow_mut(std::mem::take)
}

#[test]
fn physics_history_holds_external_edits_while_authored_motion_advances() {
    let mut runtime = runtime();
    assert_eq!(frame(&mut runtime, 0.0, 1.0, 0.0).len(), 1);
    let end = 1.0 / 30.0;
    let observations = frame(&mut runtime, end, 9.0, 3.0);
    assert_eq!(
        observations.len(),
        10,
        "drain the previous observation, eight historical samples and the live frame"
    );
    let (current, historical) = observations.split_last().unwrap();
    assert!(!current.authored_only, "live observation must restore the native policy");
    for sample in historical {
        assert!(sample.authored_only, "historical observation must retain native sampling policy");
        assert_eq!(sample.values[0], 1.0);
        assert_eq!(
            sample.values[3], 0.0,
            "new trigger must not leak into old samples"
        );
        assert_eq!(sample.values[1], sample.time.seconds.0 as f32);
        assert_eq!(sample.values[2], sample.time.beats.0 as f32);
        assert!((sample.values[4] - (sample.time.seconds.0 as f32 * 12.0).sin()).abs() < 1.0e-6);
    }
    assert_eq!(
        historical.last().unwrap().time.seconds,
        current.time.seconds,
        "close the old interval at the edit timestamp"
    );
    assert_eq!(current.values[0], 9.0);
    assert_eq!(current.values[3], 3.0);
    let next = frame(&mut runtime, 2.0 * end, 12.0, 4.0);
    assert!(
        next[..next.len() - 1]
            .iter()
            .all(|sample| sample.values[0] == 9.0 && sample.values[3] == 3.0)
    );
}

#[test]
fn physics_history_survives_compatible_generator_rebuild() {
    let mut prior = runtime();
    frame(&mut prior, 0.0, 1.0, 0.0);
    let mut rebuilt = runtime();
    rebuilt.carry_generator_state_from(&mut prior);
    let observations = frame(&mut rebuilt, 1.0 / 30.0, 9.0, 3.0);
    assert_eq!(
        observations.len(),
        10,
        "rebuild lost the open input interval"
    );
    let (current, historical) = observations.split_last().unwrap();
    assert!(
        historical
            .iter()
            .all(|sample| sample.values[0] == 1.0 && sample.values[3] == 0.0)
    );
    assert_eq!(current.values[0], 9.0);
    assert_eq!(current.values[3], 3.0);
}

fn tempo(points: &[(f64, f32)]) -> ProjectTempo {
    let mut map = TempoMap::default();
    for &(beat, bpm) in points {
        map.add_or_replace_point(Beats(beat), Bpm(bpm), TempoPointSource::Manual, 0.00001);
    }
    ProjectTempo::new(&map, Bpm(120.0))
}

/// The beat on `tempo` at `seconds`, read as a physics sample reads it.
fn sample_beat_on(tempo: &ProjectTempo, seconds: f64) -> Beats {
    TempoMapConverter::seconds_to_beat_immut(tempo.map(), Seconds(seconds + SAMPLE_BEAT_OFFSET), tempo.fallback_bpm())
}

/// Replay samples read the beat at their time plus the sample offset; the
/// frame itself carries the host's beat.
fn assert_tempo_samples(observations: &[Observation], tempo: &ProjectTempo) {
    for sample in observations {
        let expected = if sample.authored_only {
            sample_beat_on(tempo, sample.time.seconds.0)
        } else {
            TempoMapConverter::seconds_to_beat_immut(tempo.map(), sample.time.seconds, tempo.fallback_bpm())
        };
        assert_eq!(sample.time.beats, expected);
        assert_eq!(sample.values[2], expected.0 as f32);
    }
}

#[test]
fn physics_history_samples_exact_tempo_boundaries_between_display_frames() {
    let tempo = tempo(&[(0.0, 120.0), (0.03, 174.23), (0.05, 61.17)]);
    let boundaries: Vec<_> = tempo.map().points()[1..]
        .iter()
        .map(|point| {
            TempoMapConverter::beat_to_seconds_immut(tempo.map(), point.beat, tempo.fallback_bpm())
        })
        .collect();
    let mut runtime = runtime();
    runtime.set_project_tempo(Some(&tempo));
    frame(&mut runtime, -1.0 / 60.0, 1.0, 0.0);
    let samples = frame(&mut runtime, 1.0 / 30.0, 9.0, 3.0);
    assert_tempo_samples(&samples, &tempo);
    for boundary in boundaries {
        assert!(
            samples.iter().any(|sample| sample.time.seconds == boundary),
            "missing tempo boundary at {boundary:?}"
        );
    }
    assert_eq!(samples.last().unwrap().time.seconds, Seconds(1.0 / 30.0));
    assert_eq!(samples.last().unwrap().values[0], 9.0);
}

#[test]
fn physics_history_keeps_old_tempo_until_the_edit_observation() {
    let old = tempo(&[(0.0, 120.0)]);
    let edited = tempo(&[(0.0, 60.0)]);
    let mut runtime = runtime();
    runtime.set_project_tempo(Some(&old));
    frame(&mut runtime, 0.0, 1.0, 0.0);
    runtime.set_project_tempo(Some(&edited));
    let samples = frame(&mut runtime, 0.1, 9.0, 3.0);
    let (current, historical) = samples.split_last().unwrap();
    assert_tempo_samples(historical, &old);
    assert_eq!(historical.last().unwrap().time.beats, sample_beat_on(&old, 0.1));
    assert_eq!(current.time.beats, Beats(0.1));
    assert_tempo_samples(&frame(&mut runtime, 0.2, 10.0, 4.0), &edited);
}

#[test]
fn compatible_rebuild_keeps_held_tempo_and_synthetic_context_can_clear_it() {
    let old = tempo(&[(0.0, 60.0)]);
    let edited = tempo(&[(0.0, 90.0)]);
    let mut prior = runtime();
    prior.set_project_tempo(Some(&old));
    frame(&mut prior, 0.0, 1.0, 0.0);
    let mut rebuilt = runtime();
    rebuilt.carry_generator_state_from(&mut prior);
    rebuilt.set_project_tempo(Some(&edited));
    let samples = frame(&mut rebuilt, 0.1, 9.0, 3.0);
    assert_tempo_samples(&samples[..samples.len() - 1], &old);
    assert!((samples.last().unwrap().time.beats.0 - 0.15).abs() < 1e-12);
    rebuilt.set_project_tempo(None);
    assert!(rebuilt.water_ref().water.project_tempo.is_none());
    frame(&mut rebuilt, 0.1, 9.0, 3.0);
    let synthetic = frame(&mut rebuilt, 0.2, 9.0, 3.0);
    for sample in synthetic {
        // Within the sample offset: a sample's beat is read just past its time.
        assert!((sample.time.beats.0 - sample.time.seconds.0 * 2.0).abs() < 1e-8);
    }
}

#[test]
fn source_observation_uses_project_tempo_and_closes_history_once() {
    let tempo = tempo(&[(0.0, 120.0), (0.03, 60.0)]);
    let mut runtime = runtime();
    runtime.set_project_tempo(Some(&tempo));
    frame(&mut runtime, 0.0, 1.0, 0.0);
    let source = FrameTime {
        seconds: Seconds(0.02),
        beats: TempoMapConverter::seconds_to_beat_immut(
            tempo.map(),
            Seconds(0.02),
            tempo.fallback_bpm(),
        ),
        delta: Seconds(0.02),
        frame_count: 1,
    };
    observe(&mut runtime, source);
    let samples = OBSERVATIONS.with_borrow_mut(std::mem::take);
    let (at_source, replayed) = samples.split_last().unwrap();
    assert_tempo_samples(replayed, &tempo);
    assert_eq!(at_source.time.beats, source.beats, "the source keeps its own beat stamp");
    assert_eq!(samples.last().unwrap().time.seconds, source.seconds);
    assert!(samples.iter().all(|sample| !sample.draining));
    runtime.set_sim_step(manifold_water_rigid::physics::SimStep::live(manifold_core::Seconds(manifold_physics::clock::TICK)));
    let next = frame(&mut runtime, 1.0 / 30.0, 9.0, 1.0);
    assert_tempo_samples(&next, &tempo);
    assert!(
        next.iter()
            .all(|sample| sample.time.seconds > source.seconds)
    );
}

/// An audio trigger decides after its detector's lag, so its source time is
/// older than the latest frame. It is observed at that frame, never refused.
#[test]
fn source_older_than_the_latest_observation_lands_at_it() {
    let mut runtime = runtime();
    frame(&mut runtime, 0.0, 1.0, 0.0);
    frame(&mut runtime, 0.1, 1.0, 0.0);
    let latest = runtime.water_ref().water.last_frame_time.unwrap();
    OBSERVATIONS.with_borrow_mut(std::mem::take);
    let late = FrameTime { seconds: Seconds(0.06), beats: Beats(0.12), delta: Seconds::ZERO, frame_count: 1 };
    let observed = observe(&mut runtime, late);
    assert_eq!((observed.played, observed.beats), (latest.played, latest.beats));
    let samples = OBSERVATIONS.with_borrow_mut(std::mem::take);
    assert!(samples.iter().all(|sample| sample.time.seconds.0 == latest.played.seconds()), "history never reopens");
}

/// The closing observation reads the held map's beat at the sample offset,
/// as a replay sample at that time would at any frame rate; the source's
/// own observation keeps the host's beat stamp.
#[test]
fn closing_observation_reads_the_sample_beat_and_the_source_keeps_its_stamp() {
    let tempo = ProjectTempo::new(&TempoMap::default(), Bpm(137.37));
    let beat = Beats(1_000_000.25);
    let seconds = TempoMapConverter::beat_to_seconds_immut(tempo.map(), beat, tempo.fallback_bpm());
    assert_ne!(
        TempoMapConverter::seconds_to_beat_immut(tempo.map(), seconds, tempo.fallback_bpm()),
        beat,
        "fixture needs a beat-to-seconds roundtrip with rounding"
    );
    let future_map: TempoMap = serde_json::from_value(serde_json::json!({
        "points": [{"beat": 2_000_000.0, "bpm": 137.37}]
    }))
    .unwrap();
    let future_tempo = ProjectTempo::new(&future_map, Bpm(120.0));
    let source = FrameTime {
        beats: beat,
        seconds,
        delta: Seconds(1.0 / 30.0),
        frame_count: 1,
    };
    for current_tempo in [&tempo, &future_tempo] {
        let mut runtime = runtime();
        runtime.set_project_tempo(Some(&tempo));
        frame(&mut runtime, seconds.0 - 1.0 / 30.0, 1.0, 0.0);
        runtime.set_project_tempo(Some(current_tempo));
        observe(&mut runtime, source);
        let samples = OBSERVATIONS.with_borrow_mut(std::mem::take);
        let closing = &samples[samples.len() - 2];
        assert_eq!(closing.time.seconds, source.seconds);
        assert_eq!(closing.time.beats, sample_beat_on(&tempo, seconds.0));
        assert_eq!(samples.last().unwrap().time.beats, source.beats);
    }
}

#[test]
fn physics_history_reanchors_paused_edits_and_backward_seeks() {
    let mut runtime = runtime();
    frame(&mut runtime, 1.0, 1.0, 0.0);
    let paused = frame(&mut runtime, 1.0, 2.0, 1.0);
    assert_eq!(paused.len(), 1);
    let advanced = frame(&mut runtime, 1.0 + 1.0 / 60.0, 3.0, 2.0);
    assert!(
        advanced[..advanced.len() - 1]
            .iter()
            .all(|sample| sample.values[0] == 2.0 && sample.values[3] == 1.0)
    );
    let seek = frame(&mut runtime, 0.0, 4.0, 3.0);
    assert_eq!(
        seek.len(),
        1,
        "a backward seek must not replay the old interval"
    );
    let advanced = frame(&mut runtime, 1.0 / 60.0, 5.0, 4.0);
    assert!(
        advanced[..advanced.len() - 1]
            .iter()
            .all(|sample| sample.values[0] == 4.0 && sample.values[3] == 3.0)
    );
}

#[test]
fn offline_history_drain_keeps_old_controls_and_bounds_input_batches() {
    let mut runtime = runtime();
    frame(&mut runtime, 0.0, 1.0, 0.0);
    let observations = frame(&mut runtime, 3.0, 9.0, 3.0);
    let (current, historical) = observations.split_last().unwrap();
    assert!(
        !current.draining,
        "drain scope must not escape into the full frame"
    );
    assert_eq!(current.values[0], 9.0);
    assert_eq!(current.values[3], 3.0);
    assert_eq!(historical[0].time.seconds, Seconds::ZERO);
    assert!(
        historical[0].draining,
        "drain the retained preview prefix first"
    );
    let mut batch = 0;
    for sample in &historical[1..] {
        assert_eq!(sample.values[0], 1.0);
        assert_eq!(sample.values[3], 0.0);
        batch += 1;
        assert!(batch <= manifold_water_rigid::physics::AUTHORED_HISTORY_CAPACITY / 4);
        if sample.draining {
            batch = 0;
        }
    }
    assert_eq!(
        batch, 0,
        "close and drain the old interval before applying edits"
    );
    assert!(historical.len() > manifold_water_rigid::physics::AUTHORED_HISTORY_CAPACITY);
}

#[test]
fn offline_history_drain_is_never_requested_by_preview_sampling() {
    let mut runtime = runtime();
    runtime.set_sim_step(manifold_water_rigid::physics::SimStep::live(manifold_core::Seconds(manifold_physics::clock::TICK)));
    frame(&mut runtime, 0.0, 1.0, 0.0);
    let observations = frame(&mut runtime, 3.0, 9.0, 3.0);
    assert!(observations.iter().all(|sample| !sample.draining));
    assert!(observations[0].time.seconds > Seconds::ZERO);
}

/// A host instance whose Speed is audio modulated, `enabled` or not, with
/// this frame's hops as `(time, value)`.
fn modulated(enabled: bool, hops: &[(f64, f32)]) -> PresetInstance {
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    let mut instance = PresetInstance::new_generator(manifold_core::PresetTypeId::new("Modulated"));
    let send = manifold_core::audio_setup::AudioSend::new("Kick");
    let mut follow = ParameterAudioMod::new("speed".to_owned().into(), send.id, AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low));
    follow.enabled = enabled;
    follow.hop_timeline.values = hops.iter().map(|&(time, value)| HopValue { time: Seconds(time), value }).collect();
    instance.audio_mods_mut().push(follow);
    instance
}

/// The water runtime's Fire seam, as the host's Fire takes it: transport
/// `source` becomes played time with the runtime's own step.
fn observe(runtime: &mut PresetRuntime, source: FrameTime) -> HeldFrame {
    let mut water = runtime.water();
    let played = water.executor.sim_step().to_played(source.seconds);
    water.observe_physics_at_source(played, source.beats).unwrap()
}

fn empty_snapshot() -> PhysicsInputSnapshot {
    PhysicsInputSnapshot {
        values: Vec::new(),
        clock_steps: Vec::new(),
        project_tempo: None,
        tick_times: Vec::new(),
        hops: Vec::new(),
        hop_len: 0,
        hop_routes: Vec::new(),
        automation: Vec::new(),
        automation_len: 0,
        automation_routes: Vec::new(),
    }
}

/// A mod switched off and on again starts from its own new hops: a sample
/// before the first of them never hears the hop from before it was off.
#[test]
fn a_re_enabled_audio_mod_carries_no_hop_from_before_it_was_off() {
    let mut inputs = empty_snapshot();
    inputs.set_hops(Some(&modulated(true, &[(0.01, 0.9)])), Seconds::ZERO);
    inputs.set_hops(Some(&modulated(true, &[(0.03, 0.4)])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.02), Some(0.9), "a live mod carries its latest hop into the next frame");
    inputs.set_hops(Some(&modulated(false, &[])), Seconds::ZERO);
    inputs.set_hops(Some(&modulated(true, &[(0.5, 0.2)])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.45), None, "the re-enabled mod carried a hop from before it was off");
    assert_eq!(inputs.heard("speed", 0.5), Some(0.2));
}

/// A frame a transport jump landed on hands no hop from before the jump to
/// the samples after it, and all of its own hops to the replay that opens
/// at the jump's target, where a sample before the first hop reads that
/// first hop. Once that replay ran, frames carry only their latest hop, and
/// a second call in one tick (a Fire, then the frame) carries what the
/// first carried.
#[test]
fn a_transport_jump_drops_the_carried_hop_and_hands_its_own_to_the_opening() {
    let mut inputs = empty_snapshot();
    inputs.set_hops(Some(&modulated(true, &[(0.01, 0.9)])), Seconds::ZERO);
    inputs.set_hops(Some(&modulated(true, &[])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.02), Some(0.9));
    inputs.jump_hops(true);
    assert_eq!(inputs.heard("speed", 0.02), None, "the hop from before the jump reached a sample after it");
    inputs.set_hops(Some(&modulated(true, &[(0.9, 0.3)])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.85), Some(0.3), "the opening reads the first hop after the jump");
    let mut inputs = empty_snapshot();
    inputs.set_hops(Some(&modulated(true, &[(0.01, 0.9)])), Seconds::ZERO);
    inputs.set_hops(Some(&modulated(true, &[(0.85, 0.7), (0.86, 0.6)])), Seconds::ZERO);
    inputs.jump_hops(true);
    inputs.set_hops(Some(&modulated(true, &[(0.9, 0.3)])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.855), Some(0.7), "the opening replay reads the jump frame's hops at their own times");
    assert_eq!(inputs.heard("speed", 0.87), Some(0.6));
    assert_eq!(inputs.heard("speed", 0.9), Some(0.3));
    inputs.hops_replayed();
    inputs.set_hops(Some(&modulated(true, &[(0.95, 0.1)])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.855), None, "a later frame carries only the latest hop");
    assert_eq!(inputs.heard("speed", 0.92), Some(0.3));
    inputs.set_hops(Some(&modulated(true, &[(0.95, 0.1)])), Seconds::ZERO);
    assert_eq!(inputs.heard("speed", 0.92), Some(0.3), "a second call in one tick keeps the first's carry");
}

/// A Fire handled after a forward seek, before the seek's frame renders,
/// lands at the latest observation: the skipped span is never replayed, and
/// the next replay opens at the seek's target, at every rate.
#[test]
fn a_fire_after_a_forward_seek_never_replays_the_skipped_span() {
    const TARGET: f64 = 0.9137;
    let interval = Seconds(manifold_physics::clock::TICK);
    for fps in [30_u32, 60, 120, 144] {
        for offline in [false, true] {
            let step = || if offline { SimStep::export(interval) } else { SimStep::live(interval) };
            let fps_f = f64::from(fps);
            let mut runtime = runtime();
            runtime.set_sim_step(step());
            for k in 0..=(0.5 * fps_f).round() as u32 {
                frame(&mut runtime, f64::from(k) / fps_f, 1.0, 0.0);
            }
            let landed = (TARGET * fps_f - 2e-7).ceil() / fps_f;
            runtime.set_sim_step(step().with_played_target(Some(PlayedTime::from_played(TARGET))));
            let source = FrameTime { seconds: Seconds(landed), beats: Beats(landed * 2.0), delta: Seconds::ZERO, frame_count: 1 };
            observe(&mut runtime, source);
            let mut seen = OBSERVATIONS.with_borrow_mut(std::mem::take);
            seen.extend(frame(&mut runtime, landed, 1.0, 1.0));
            runtime.set_sim_step(step());
            let next = frame(&mut runtime, landed + 1.0 / fps_f, 1.0, 1.0);
            let at = |sample: &Observation| sample.time.seconds.0;
            let skipped = seen.iter().chain(&next).find(|sample| at(sample) > 0.5 + 1e-12 && at(sample) < TARGET - 1e-12);
            assert!(skipped.is_none(), "{fps} fps, offline {offline}: a sample in the skipped span at {:?}", skipped.map(at));
            assert_eq!(at(&next[0]), TARGET, "{fps} fps, offline {offline}: the replay opens at the seek's target");
        }
    }
}

/// Fires observed while the BPM is dragged up and down, one edit a frame,
/// keep their source on played time: each lands after the one before and is
/// never moved back by an edit.
#[test]
fn fire_sources_hold_across_a_bpm_drag() {
    let interval = Seconds(manifold_physics::clock::TICK);
    let mut runtime = runtime();
    let (mut seconds, mut shift) = (1.0, 0.0);
    let mut sources = Vec::new();
    for k in 0..30_i64 {
        // Up (the transport moves back further than a frame) for ten
        // frames, then down.
        let moved = match k { 0 => 0.0, 1..10 => -0.03, _ => 0.02 };
        seconds += moved;
        shift += moved;
        runtime.set_sim_step(SimStep::live(interval).with_tempo_shift(Seconds(shift)));
        frame(&mut runtime, seconds, 1.0, 0.0);
        let at = seconds + 1.0 / 120.0;
        let source = FrameTime { seconds: Seconds(at), beats: Beats(at * 2.0), delta: Seconds::ZERO, frame_count: k };
        sources.push(observe(&mut runtime, source).played.seconds());
        seconds += 1.0 / 60.0;
    }
    assert!(sources.windows(2).all(|pair| pair[1] > pair[0]), "{sources:?}");
}
