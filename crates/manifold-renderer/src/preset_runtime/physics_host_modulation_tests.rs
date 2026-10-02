//! Host audio modulation reaches a GPU liquid's force once per liquid tick,
//! whatever the display rate (BUG-2jx6 (host-fed modulation sampled per
//! liquid tick)). Drives real playback modulation into a Uniform Force chain.
use super::*;
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::{EffectNode, EffectNodeContext, EffectNodeType, ParamDef};
use manifold_core::audio_features::{
    AudioFeatureHop, AudioFeatureSnapshot, AudioHopBatch, AudioHopStamp, SendFeatures,
};
use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
use manifold_core::audio_setup::AudioSend;
use manifold_core::audio_trigger::FireMeterCapture;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::layer::Layer;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_physics::VectorField;
use std::{borrow::Cow, cell::RefCell};

const TICK_RATE: f64 = 120.0;
const SAMPLE_RATE: u32 = 48_000;
const HOP: u64 = 480;

thread_local! {
    static TICKS: RefCell<Vec<(f64, f32)>> = const { RefCell::new(Vec::new()) };
}

/// Stands in for a GPU liquid: asks for its tick starts and records the
/// acceleration it sees at each one.
struct TickedLiquid(EffectNodeType, Vec<f64>);

impl EffectNode for TickedLiquid {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.0
    }
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: Cow::Borrowed("acceleration_field"),
            ty: PortType::VectorField,
            kind: PortKind::Input,
            required: false,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    /// Like the liquid's field history: request each tick start in
    /// `(from, until]`; the first sample at or after it records that tick.
    fn request_physics_samples(&mut self, from: f64, until: f64, out: &mut Vec<f64>) {
        let mut tick = (from * TICK_RATE + 1e-9).floor() + 1.0;
        while tick / TICK_RATE <= until + 1e-12 {
            out.push(tick / TICK_RATE);
            self.1.push(tick / TICK_RATE);
            tick += 1.0;
        }
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if !crate::node_graph::physics::authored_sample_only() {
            return;
        }
        let now = ctx.time.seconds.0;
        let reached = self.1.partition_point(|&tick| tick <= now);
        if reached == 0 {
            return;
        }
        let force = ctx
            .inputs
            .vector_field("acceleration_field")
            .map_or(f32::NAN, |field| field.sample([0.0; 3])[1]);
        TICKS.with_borrow_mut(|ticks| {
            ticks.extend(self.1.drain(..reached).map(|tick| (tick, force)));
        });
    }
}

fn runtime() -> PresetRuntime {
    let liquid = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(liquid, move || Box::new(TickedLiquid(EffectNodeType::new(liquid), Vec::new())));
    let def = serde_json::json!({
        "version": 2, "name": "Kick force",
        "presetMetadata": {
            "id": "KickForce", "displayName": "Kick force", "category": "Test",
            "oscPrefix": "kick_force", "available": true,
            "params": [{"id": "strength", "name": "Strength", "min": -20.0, "max": 20.0,
                "defaultValue": 1.0}],
            "bindings": [{"id": "strength", "label": "Strength", "defaultValue": 1.0,
                "target": {"kind": "node", "nodeId": "force_strength", "param": "strength"}}]
        },
        "nodes": [
            {"id": 0, "nodeId": "force_field", "typeId": "node.uniform_vector_field", "params": {
                "x": {"type": "Float", "value": 0.0},
                "y": {"type": "Float", "value": 1.0},
                "z": {"type": "Float", "value": 0.0}
            }},
            {"id": 1, "nodeId": "force_strength", "typeId": "node.scale_vector_field"},
            {"id": 2, "nodeId": "liquid", "typeId": liquid},
            {"id": 3, "nodeId": "source", "typeId": "system.source"},
            {"id": 4, "nodeId": "output", "typeId": "system.final_output"},
            {"id": 5, "nodeId": "input", "typeId": "system.generator_input"}
        ],
        "wires": [
            {"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "field"},
            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "acceleration_field"},
            {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"}
        ]
    });
    PresetRuntime::from_json_str(&def.to_string(), &registry).unwrap()
}

fn project() -> Project {
    let mut project = Project::default();
    let send = AudioSend::new("Kick");
    let send_id = send.id.clone();
    project.audio_setup.sends.push(send);
    let mut layer = Layer::new_generator("Liquid".into(), PresetTypeId::new("KickForce"), 0);
    let generator = layer.gen_params_or_init();
    let mut strength = Param::bundled(ParamSpecDef {
        id: "strength".into(),
        name: "Strength".into(),
        min: -20.0,
        max: 20.0,
        default_value: 1.0,
        ..Default::default()
    });
    strength.value = 1.0;
    strength.base = 1.0;
    generator.params = ParamManifest::from_params(vec![strength]);
    let mut kick = ParameterAudioMod::new(
        "strength".into(),
        send_id,
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low),
    );
    kick.shape.attack_ms = 20.0;
    kick.shape.release_ms = 60.0;
    generator.audio_mods_mut().push(kick);
    project.timeline.layers = vec![layer];
    project
}

/// A kick on the 10 ms hop grid from 0.5 s, after every rate's live clock
/// has settled onto a frame that lands exactly on a hop.
fn level(end_sample: u64) -> f32 {
    match end_sample / HOP {
        51..=53 => 1.0,
        54 => 0.6,
        60..=61 => 0.8,
        _ => 0.0,
    }
}

fn snapshot(hops: std::ops::Range<u64>, offline: bool) -> AudioFeatureSnapshot {
    let mut batch = AudioHopBatch::with_capacity(64);
    batch.begin(1);
    for hop in hops {
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
                    timeline_time: offline
                        .then(|| Seconds(end_sample as f64 / f64::from(SAMPLE_RATE))),
                },
                dt: Seconds(HOP as f64 / f64::from(SAMPLE_RATE)),
                features,
            })
            .unwrap();
    }
    AudioFeatureSnapshot {
        sends: vec![SendFeatures::default()],
        hop_batches: vec![batch],
        ..Default::default()
    }
}

fn run(fps: u32, offline: bool) -> Vec<(f64, f32)> {
    TICKS.with_borrow_mut(Vec::clear);
    let mut project = project();
    let mut runtime = runtime();
    let mut next_hop = 1;
    for frame in 0..=fps {
        let seconds = f64::from(frame) / f64::from(fps);
        // Hops whose audio ended by this frame are delivered with it.
        let delivered = (seconds * f64::from(SAMPLE_RATE) / HOP as f64 + 1e-9).floor() as u64 + 1;
        let audio = snapshot(next_hop..delivered.max(next_hop), offline);
        next_hop = delivered.max(next_hop);
        manifold_playback::modulation::evaluate_modulation(
            &mut project,
            Beats(seconds * 2.0),
            Seconds(seconds),
            Seconds(1.0 / f64::from(fps)),
            &audio,
            &mut Vec::new(),
            &mut Vec::new(),
            &[],
            &mut FireMeterCapture::default(),
        );
        let generator = project.timeline.layers[0].gen_params().unwrap();
        runtime.set_physics_source_instance(Some(generator));
        runtime.apply_param_values(&generator.params);
        runtime.execute_frame(FrameTime {
            seconds: Seconds(seconds),
            beats: Beats(seconds * 2.0),
            delta: Seconds(if frame == 0 { 0.0 } else { 1.0 / f64::from(fps) }),
            frame_count: i64::from(frame),
        });
    }
    TICKS.with_borrow_mut(std::mem::take)
}

#[test]
fn host_kick_forces_per_tick_match_across_frame_rates() {
    let baseline = run(60, true);
    assert_eq!(baseline.len(), TICK_RATE as usize, "one force per tick");
    let kick: Vec<f32> = baseline
        .iter()
        .filter(|(time, _)| (0.5..0.7).contains(time))
        .map(|&(_, force)| force)
        .collect();
    assert!(
        kick.windows(2).filter(|w| w[0] != w[1]).count() > 6,
        "the kick must shape the force within frames, got {kick:?}"
    );
    for fps in [24, 30, 60] {
        for offline in [true, false] {
            let ticks = run(fps, offline);
            let pairs = ticks.iter().zip(&baseline);
            for (&(time, force), &(base_time, base_force)) in pairs {
                assert!((time - base_time).abs() < 1e-9, "{fps} fps offline={offline}");
                assert_eq!(force, base_force, "{fps} fps offline={offline} at {time}");
            }
            assert_eq!(ticks.len(), baseline.len(), "{fps} fps offline={offline}");
        }
    }
}
