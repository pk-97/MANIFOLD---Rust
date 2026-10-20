//! A host Speed param ramped by an arrangement automation lane reaches a
//! simulation clock at each tick's start, whatever the display rate, live or
//! exported (BUG-mjm2o (host-automated Speed held across the replay)). Drives
//! real playback automation into a generator whose Speed is a host param.
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};
use manifold_core::layer::Layer;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::project::Project;
use manifold_core::{tempo::TempoMap, units::Bpm, Beats, PresetTypeId, Seconds};
use manifold_core::audio_features::AudioFeatureSnapshot;
use manifold_core::audio_trigger::FireMeterCapture;
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

/// Stands in for a GPU liquid's clock: observes its Speed param in each
/// replay sample, as the domains do, and runs the frame's ticks.
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
        &[]
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

fn runtime() -> PresetRuntime {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(LIQUID, || {
        let cell_size = NodePort { name: Cow::Borrowed("cell_size"), ty: PortType::Scalar(ScalarType::F32), kind: PortKind::Output, required: false };
        Box::new(TickedClock(EffectNodeType::new(LIQUID), SimulationClock::default(), [cell_size]))
    });
    let def = serde_json::json!({
        "version": 2, "name": "Automated speed",
        "presetMetadata": {
            "id": "AutomatedSpeed", "displayName": "Automated speed", "category": "Test",
            "oscPrefix": "automated_speed", "available": true,
            "params": [{"id": "speed", "name": "Speed", "min": 0.0, "max": 4.0, "defaultValue": 1.0}],
            "bindings": [{"id": "speed", "label": "Speed", "defaultValue": 1.0,
                "target": {"kind": "node", "nodeId": "clock", "param": "speed"}}]
        },
        "nodes": [
            {"id": 0, "nodeId": "clock", "typeId": LIQUID},
            {"id": 3, "nodeId": "source", "typeId": "system.source"},
            {"id": 4, "nodeId": "output", "typeId": "system.final_output"},
            {"id": 5, "nodeId": "input", "typeId": "system.generator_input"}
        ],
        "wires": [{"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"}]
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

/// Speed's lane: 0.5 at beat 0 rising linearly to 2.0 at beat 4 (2 s at
/// 120 BPM), so Speed is 0.5 + 0.75 t.
fn project() -> Project {
    let mut project = Project::default();
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
    let point = |beat, value| AutomationPoint { beat: Beats(beat), value, shape: SegmentShape::Linear };
    generator.automation_lanes = Some(vec![AutomationLane {
        param_id: "speed".to_owned().into(),
        enabled: true,
        points: vec![point(0.0, 0.5), point(4.0, 2.0)],
    }]);
    project.timeline.layers = vec![layer];
    project
}

/// One second at `fps`, as the host plays it: automation, then modulation,
/// then the frame. The frame's beat carries the engine's sampling offset.
fn run(fps: u32, offline: bool) -> Vec<(u64, u64)> {
    TICKS.with_borrow_mut(Vec::clear);
    let mut runtime = runtime();
    let mut project = project();
    let tempo = ProjectTempo::new(&TempoMap::default(), Bpm(120.0));
    let (mut latches, mut gestures) = (AutomationLatches::default(), AutomationGestures::default());
    let mode = if offline { AutomationSamplingMode::Export } else { AutomationSamplingMode::Playback };
    for frame in 0..=fps {
        let seconds = f64::from(frame) / f64::from(fps);
        let beat = Beats((seconds + 1e-7 / f64::from(fps)) * 2.0);
        manifold_playback::automation::evaluate_all_automation_with_mode(&mut project, beat, &mut latches, false, &mut gestures, mode);
        manifold_playback::modulation::evaluate_modulation(
            &mut project,
            beat,
            Seconds(seconds),
            Seconds(1.0 / f64::from(fps)),
            &AudioFeatureSnapshot::default(),
            &manifold_playback::clip_controls::ClipControlFrame::default(),
            &mut Vec::new(),
            &mut FireMeterCapture::default(),
        );
        let generator = project.timeline.layers[0].gen_params().unwrap();
        runtime.set_project_tempo(Some(&tempo));
        runtime.set_source_instance(Some(generator));
        runtime.apply_param_values(&generator.params);
        runtime.set_sim_step(if offline { SimStep::export(Seconds(TICK)) } else { SimStep::live(Seconds(TICK)) });
        runtime.execute_frame(FrameTime {
            seconds: Seconds(seconds),
            beats: beat,
            delta: Seconds(if frame == 0 { 0.0 } else { 1.0 / f64::from(fps) }),
            frame_count: i64::from(frame),
        });
    }
    TICKS.with_borrow_mut(std::mem::take)
}

#[test]
fn automated_speed_ticks_the_same_at_30_60_120_144_fps_live_and_export() {
    let reference = run(60, true);
    assert_eq!(reference.len(), 60, "one second of 60 Hz ticks");
    for (k, &(start, end)) in reference.iter().enumerate() {
        let length = f64::from_bits(end) - f64::from_bits(start);
        let expected = TICK * (0.5 + 0.75 * k as f64 * TICK);
        assert!((length - expected).abs() < 1e-7, "tick {k} runs at the lane's Speed at its start: {length} against {expected}");
    }
    for fps in [30, 60, 120, 144] {
        for offline in [false, true] {
            assert!(run(fps, offline) == reference, "{fps} fps, offline {offline}: tick lengths differ");
        }
    }
}
