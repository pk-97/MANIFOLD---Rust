use crate::water::runtime::WaterRuntimeExt;
use crate::parameters::ParamValue;
use crate::persistence::PrimitiveRegistry;
use crate::runtime::FrameContextInputs;
use crate::runtime::PresetRuntime;
use manifold_core::Beats;
use manifold_core::NodeId;
use manifold_core::Seconds;
use crate::exec::effect_node::FrameTime;
use super::*;
use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use crate::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, parameters::ParamDef};
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
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
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
                draining: crate::water::physics::history_drain_requested(),
                authored_only: crate::water::physics::authored_sample_only(),
            })
        });
    }
}

fn runtime() -> PresetRuntime {
    OBSERVATIONS.with_borrow_mut(Vec::clear);
    let mut registry = PrimitiveRegistry::with_builtin();
    crate::testkit::physics_fixtures::register(&mut registry);
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
                .map_or(0.0, |previous| seconds - previous.seconds.0),
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
    assert!(!crate::water::physics::authored_sample_only());
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

fn assert_tempo_samples(observations: &[Observation], tempo: &ProjectTempo) {
    for sample in observations {
        let expected = TempoMapConverter::seconds_to_beat_immut(
            tempo.map(),
            sample.time.seconds,
            tempo.fallback_bpm(),
        );
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
    assert_eq!(historical.last().unwrap().time.beats, Beats(0.2));
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
        assert!((sample.time.beats.0 - sample.time.seconds.0 * 2.0).abs() < 1e-12);
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
    runtime.water().observe_physics_at_source(source).unwrap();
    let samples = OBSERVATIONS.with_borrow_mut(std::mem::take);
    assert_tempo_samples(&samples, &tempo);
    assert_eq!(samples.last().unwrap().time.seconds, source.seconds);
    assert!(samples.iter().all(|sample| !sample.draining));
    let _preview = crate::water::physics::PhysicsStepScope::for_render(false);
    let next = frame(&mut runtime, 1.0 / 30.0, 9.0, 1.0);
    assert_tempo_samples(&next, &tempo);
    assert!(
        next.iter()
            .all(|sample| sample.time.seconds > source.seconds)
    );
}

#[test]
fn unchanged_tempo_preserves_external_beat_authority_at_the_closing_observation() {
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
        runtime.water().observe_physics_at_source(source).unwrap();
        let samples = OBSERVATIONS.with_borrow_mut(std::mem::take);
        let closing = &samples[samples.len() - 2];
        assert_eq!(closing.time.seconds, source.seconds);
        assert_eq!(
            closing.time.beats, source.beats,
            "roundtrip drift must not produce a second clock stamp at the same second"
        );
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
        assert!(batch <= crate::water::physics::AUTHORED_HISTORY_CAPACITY / 4);
        if sample.draining {
            batch = 0;
        }
    }
    assert_eq!(
        batch, 0,
        "close and drain the old interval before applying edits"
    );
    assert!(historical.len() > crate::water::physics::AUTHORED_HISTORY_CAPACITY);
}

#[test]
fn offline_history_drain_is_never_requested_by_preview_sampling() {
    let _preview = crate::water::physics::PhysicsStepScope::for_render(false);
    let mut runtime = runtime();
    frame(&mut runtime, 0.0, 1.0, 0.0);
    let observations = frame(&mut runtime, 3.0, 9.0, 3.0);
    assert!(observations.iter().all(|sample| !sample.draining));
    assert!(observations[0].time.seconds > Seconds::ZERO);
}
