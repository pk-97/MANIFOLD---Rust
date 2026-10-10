//! Host audio modulation reaches a GPU liquid's force once per liquid tick,
//! whatever the display rate (BUG-2jx6 (host-fed modulation sampled per
//! liquid tick)). Drives real playback modulation into a Uniform Force chain.
use manifold_node_engine::exec::effect_node::FrameTime;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::*;
use manifold_core::{Beats, Seconds, PresetTypeId};
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
use manifold_node_engine::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, parameters::ParamDef};
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
use manifold_physics::{FieldValue, VectorField};
use std::{borrow::Cow, cell::RefCell};

const TICK_RATE: f64 = 120.0;
const SAMPLE_RATE: u32 = 48_000;
const HOP: u64 = 480;

thread_local! {
    static TICKS: RefCell<Vec<(f64, f32)>> = const { RefCell::new(Vec::new()) };
}

/// Stands in for a GPU liquid: asks for its tick starts and records the
/// acceleration it sees at each one. A surface built from its `cell_size`
/// output lets a scene object recognise it as water.
struct TickedLiquid(EffectNodeType, Vec<f64>, [NodeOutput; 1]);

impl EffectNode for TickedLiquid {
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
            name: Cow::Borrowed("acceleration_field"),
            ty: PortType::VectorField,
            kind: PortKind::Input,
            required: false,
        }];
        &INPUTS
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.2
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if !ctx.sim_step.authored_sample_only {
            return;
        }
        let now = ctx.time.seconds.0;
        let reached = self.1.partition_point(|&tick| tick <= now);
        if reached == 0 {
            return;
        }
        let force = ctx
            .inputs
            .cpu_value::<FieldValue>("acceleration_field")
            .map_or(f32::NAN, |field| field.sample([0.0; 3])[1]);
        TICKS.with_borrow_mut(|ticks| {
            ticks.extend(self.1.drain(..reached).map(|tick| (tick, force)));
        });
    }
}

impl manifold_water_rigid::node::PhysicsNode for TickedLiquid {
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
}

inventory::submit! {
    manifold_water_rigid::node::PhysicsNodeRegistration::new::<TickedLiquid>()
}

const LIQUID: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(LIQUID, || {
        let cell_size = NodePort {
            name: Cow::Borrowed("cell_size"),
            ty: PortType::Scalar(manifold_node_engine::ports::ScalarType::F32),
            kind: PortKind::Output,
            required: false,
        };
        Box::new(TickedLiquid(EffectNodeType::new(LIQUID), Vec::new(), [cell_size]))
    });
    registry
}

/// A generator whose own `strength` param scales the liquid's force.
fn generator() -> (PresetRuntime, &'static str) {
    let liquid = LIQUID;
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
    (PresetRuntime::from_json_str(&def.to_string(), &registry()).unwrap(), "strength")
}

/// A Uniform Force card on a scene whose water is the liquid; the card's
/// strength is a host param on the owner, as on stage.
fn modifier_card() -> (PresetRuntime, String) {
    use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    let owner: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 2, "name": "Kick water",
        "presetMetadata": {
            "id": "KickForce", "displayName": "Kick water", "category": "Test",
            "oscPrefix": "kick_water", "available": true, "params": [], "bindings": []
        },
        "nodes": [
            {"id": 2, "nodeId": "liquid", "typeId": LIQUID},
            {"id": 3, "nodeId": "source", "typeId": "system.source"},
            {"id": 4, "nodeId": "output", "typeId": "system.final_output"},
            {"id": 5, "nodeId": "input", "typeId": "system.generator_input"},
            {"id": 6, "nodeId": "water_object", "typeId": "node.scene_object"},
            {"id": 7, "nodeId": "scene", "typeId": "node.render_scene"},
            {"id": 8, "nodeId": "surface", "typeId": "node.plane_mesh"}
        ],
        "wires": [
            {"fromNode": 2, "fromPort": "cell_size", "toNode": 8, "toPort": "width"},
            {"fromNode": 8, "fromPort": "vertices", "toNode": 6, "toPort": "vertices"},
            {"fromNode": 6, "fromPort": "object", "toNode": 7, "toPort": "object_0"},
            {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in"}
        ]
    }))
    .unwrap();
    let recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/UniformForce.json"
    )))
    .unwrap();
    let top = |node: &str| SceneNodeRef { scope: Vec::new(), node: manifold_core::NodeId::new(node) };
    let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        &owner,
        &recipe,
        manifold_core::NodeId::new("kick"),
        top("scene"),
        SceneTargetSelection::Explicit { objects: vec![top("water_object")] },
    )
    .unwrap();
    let def = manifold_core::scene_modifier_edit::insert_scene_modifier(&owner, 0, instance)
        .unwrap()
        .graph;
    let strength = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| {
            matches!(&binding.target,
                BindingTarget::SceneModifier { param_id, .. } if param_id == "strength")
        })
        .unwrap()
        .id
        .clone();
    let json = serde_json::to_string(&def).unwrap();
    (PresetRuntime::from_json_str(&json, &registry()).unwrap(), strength)
}

fn project(strength_id: &str) -> Project {
    let mut project = Project::default();
    let send = AudioSend::new("Kick");
    let send_id = send.id.clone();
    project.audio_setup.sends.push(send);
    let mut layer = Layer::new_generator("Liquid".into(), PresetTypeId::new("KickForce"), 0);
    let generator = layer.gen_params_or_init();
    let mut strength = Param::bundled(ParamSpecDef {
        id: strength_id.into(),
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
        strength_id.to_owned().into(),
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

fn run<S: AsRef<str>>(
    build: fn() -> (PresetRuntime, S),
    fps: u32,
    offline: bool,
) -> Vec<(f64, f32)> {
    TICKS.with_borrow_mut(Vec::clear);
    let (mut runtime, strength) = build();
    let mut project = project(strength.as_ref());
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
            &manifold_playback::clip_controls::ClipControlFrame::default(),
            &mut Vec::new(),
            &mut FireMeterCapture::default(),
        );
        let generator = project.timeline.layers[0].gen_params().unwrap();
        runtime.set_source_instance(Some(generator));
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
    assert_identical_across_frame_rates(generator);
}

#[test]
fn modifier_card_kick_forces_per_tick_match_across_frame_rates() {
    assert_identical_across_frame_rates(modifier_card);
}

fn assert_identical_across_frame_rates<S: AsRef<str>>(build: fn() -> (PresetRuntime, S)) {
    let run = |fps, offline| run(build, fps, offline);
    let baseline = run(60, true);    assert_eq!(baseline.len(), TICK_RATE as usize, "one force per tick");
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

/// At 40 fps a display frame holds three 120 Hz liquid ticks. Each tick of a
/// Uniform Force card reads the kick envelope at its own time: inside one
/// frame the three ticks differ, and every tick's force is the card's gain
/// times the latest hop value at or before that tick.
#[test]
fn modifier_card_kick_is_sampled_at_each_tick_inside_one_frame() {
    TICKS.with_borrow_mut(Vec::clear);
    let (mut runtime, strength) = modifier_card();
    let mut project = project(&strength);
    let fps = 40u32;
    let mut hops: Vec<manifold_core::audio_mod::HopValue> = Vec::new();
    let mut next_hop = 1;
    for frame in 0..=fps {
        let seconds = f64::from(frame) / f64::from(fps);
        let delivered = (seconds * f64::from(SAMPLE_RATE) / HOP as f64 + 1e-9).floor() as u64 + 1;
        let audio = snapshot(next_hop..delivered.max(next_hop), false);
        next_hop = delivered.max(next_hop);
        manifold_playback::modulation::evaluate_modulation(
            &mut project,
            Beats(seconds * 2.0),
            Seconds(seconds),
            Seconds(1.0 / f64::from(fps)),
            &audio,
            &manifold_playback::clip_controls::ClipControlFrame::default(),
            &mut Vec::new(),
            &mut FireMeterCapture::default(),
        );
        let generator = project.timeline.layers[0].gen_params().unwrap();
        hops.extend_from_slice(&generator.audio_mods.as_deref().unwrap()[0].hop_timeline.values);
        runtime.set_source_instance(Some(generator));
        runtime.apply_param_values(&generator.params);
        runtime.execute_frame(FrameTime {
            seconds: Seconds(seconds),
            beats: Beats(seconds * 2.0),
            delta: Seconds(if frame == 0 { 0.0 } else { 1.0 / f64::from(fps) }),
            frame_count: i64::from(frame),
        });
    }
    let ticks = TICKS.with_borrow_mut(std::mem::take);
    assert_eq!(ticks.len(), TICK_RATE as usize, "one force per tick");
    let strength_at = |time: f64| {
        let n = hops.partition_point(|hop| hop.time.0 <= time + 1e-12);
        n.checked_sub(1).map_or(1.0, |i| hops[i].value)
    };
    // Before the kick the strength is its base value 1.0.
    let gain = ticks[0].1;
    assert!(gain.abs() > 0.0);
    for &(time, force) in &ticks {
        let expected = gain * strength_at(time);
        assert!((force - expected).abs() <= 1e-5 * expected.abs().max(1.0), "tick {time}: {force} vs {expected}");
    }
    let distinct_frame = (0..fps).any(|frame| {
        let (start, end) = (f64::from(frame) / f64::from(fps), f64::from(frame + 1) / f64::from(fps));
        let inside: Vec<f32> = ticks.iter()
            .filter(|(time, _)| *time > start + 1e-9 && *time <= end + 1e-9)
            .map(|&(_, force)| force)
            .collect();
        inside.len() == 3 && inside[0] != inside[1] && inside[1] != inside[2] && inside[0] != inside[2]
    });
    assert!(distinct_frame, "some frame's three ticks must each see a different kick value: {ticks:?}");
}

use manifold_core::params::ParamManifest;
