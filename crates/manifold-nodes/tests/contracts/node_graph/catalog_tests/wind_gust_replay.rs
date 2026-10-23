//! A Wind card with Turbulence on, over PhysicsSolids plus a liquid and a
//! swarm: the drifting gusts build into the physics history replay, and
//! every tick reads the same gust whichever consumer asks, at any display
//! rate, live or in export, and across a compatible rebuild.
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::scene_modifier_preset::SceneTargetSelection;
use manifold_core::{Beats, NodeId, PlayedTime, Seconds};
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime, SimStep};
use manifold_node_engine::parameters::ParamDef;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::runtime::*;
use manifold_physics::{FieldValue, VectorField};
use std::{borrow::Cow, cell::RefCell};

/// One consumer's force at one point at one tick start, as bits.
type Reading = (&'static str, u64, [u32; 6]);

thread_local! {
    static READINGS: RefCell<Vec<Reading>> = const { RefCell::new(Vec::new()) };
}

const LIQUID: &str = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
const SWARM: &str = "test.ticked_swarm";
const POINTS: [[f32; 3]; 2] = [[0.4, 1.3, -0.6], [-1.1, 0.7, 0.9]];

/// Stands in for a GPU liquid or a swarm's forces node: asks for its own
/// tick starts and records the force it is handed at each one.
struct TickedConsumer {
    kind: EffectNodeType,
    name: &'static str,
    rate: f64,
    swarm: bool,
    pending: Vec<f64>,
}

impl EffectNode for TickedConsumer {
    fn is_liveness_root(&self) -> bool {
        true
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.kind
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
        // A liquid is recognised by the surface built from its cell size,
        // a swarm by the copies it places.
        static OUTPUTS: [NodeOutput; 2] = [
            NodePort { name: Cow::Borrowed("cell_size"), ty: PortType::Scalar(ScalarType::F32), kind: PortKind::Output, required: false },
            NodePort {
                name: Cow::Borrowed("copies"),
                ty: PortType::Array(ArrayType::of_known::<InstanceTransform>()),
                kind: PortKind::Output,
                required: false,
            },
        ];
        &OUTPUTS
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn acceleration_input(&self) -> Option<&'static str> {
        self.swarm.then_some("acceleration_field")
    }
    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        if !ctx.sim_step.authored_sample_only {
            return;
        }
        let now = ctx.played().seconds();
        let reached = self.pending.partition_point(|&tick| tick <= now);
        if reached == 0 {
            return;
        }
        let field = ctx.inputs.cpu_value::<FieldValue>("acceleration_field").unwrap_or_else(|| panic!("the wind reaches the {} at {}", self.name, ctx.time.seconds.0));
        let reading = POINTS.map(|point| field.sample_force(point));
        let bits = [
            reading[0].push[0], reading[0].drag_wind[0], reading[0].drag_wind[2],
            reading[1].push[1], reading[1].drag_wind[1], reading[1].drag,
        ]
        .map(f32::to_bits);
        READINGS.with_borrow_mut(|readings| {
            readings.extend(self.pending.drain(..reached).map(|tick| (self.name, tick.to_bits(), bits)));
        });
    }
}

impl manifold_nodes_physics::tick_sampled::TickSampled for TickedConsumer {
    fn request_physics_samples(&mut self, from: PlayedTime, until: PlayedTime, out: &mut Vec<PlayedTime>) {
        let (from, until) = (from.seconds(), until.seconds());
        let mut tick = (from * self.rate + 1e-9).floor() + 1.0;
        while tick / self.rate <= until + 1e-12 {
            out.push(PlayedTime::from_played(tick / self.rate));
            self.pending.push(tick / self.rate);
            tick += 1.0;
        }
    }
}

inventory::submit! {
    manifold_nodes_physics::tick_sampled::TickSampledRegistration::new::<TickedConsumer>()
}

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    // The liquid ticks at 120 Hz and the swarm at 90 Hz: they share every
    // thirtieth of a second and interleave in between.
    registry.register(LIQUID, || {
        Box::new(TickedConsumer { kind: EffectNodeType::new(LIQUID), name: "liquid", rate: 120.0, swarm: false, pending: Vec::new() })
    });
    registry.register(SWARM, || {
        Box::new(TickedConsumer { kind: EffectNodeType::new(SWARM), name: "swarm", rate: 90.0, swarm: true, pending: Vec::new() })
    });
    registry
}

/// PhysicsSolids with a liquid and a swarm added to its scene, under one
/// Wind card on every object with Turbulence on.
fn windy_scene() -> String {
    let mut doc: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/presets/PhysicsSolids.json"
    )))
    .unwrap();
    for (offset, (node_id, type_id)) in [("liquid", LIQUID), ("swarm", SWARM)].into_iter().enumerate() {
        let id = 900 + 10 * offset as u64;
        let nodes = doc["nodes"].as_array_mut().unwrap();
        nodes.push(serde_json::json!({"id": id, "nodeId": node_id, "typeId": type_id}));
        nodes.push(serde_json::json!({"id": id + 1, "nodeId": format!("{node_id}_surface"), "typeId": "node.plane_mesh"}));
        nodes.push(serde_json::json!({"id": id + 2, "nodeId": format!("{node_id}_object"), "typeId": "node.scene_object"}));
        let wires = doc["wires"].as_array_mut().unwrap();
        if node_id == "liquid" {
            wires.push(serde_json::json!({"fromNode": id, "fromPort": "cell_size", "toNode": id + 1, "toPort": "width"}));
        } else {
            wires.push(serde_json::json!({"fromNode": id, "fromPort": "copies", "toNode": id + 2, "toPort": "instances"}));
        }
        wires.push(serde_json::json!({"fromNode": id + 1, "fromPort": "vertices", "toNode": id + 2, "toPort": "vertices"}));
        wires.push(serde_json::json!({"fromNode": id + 2, "fromPort": "object", "toNode": 30, "toPort": format!("object_{}", 6 + offset)}));
    }
    let scene = doc["nodes"].as_array_mut().unwrap().iter_mut().find(|node| node["typeId"] == "node.render_scene").unwrap();
    scene["params"]["objects"]["value"] = 8.0.into();
    let owner: EffectGraphDef = serde_json::from_value(doc).unwrap();

    let mut wind: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/scene-modifier-presets/Wind.json"
    )))
    .unwrap();
    let metadata = wind.preset_metadata.as_mut().unwrap();
    for (id, value) in [("turbulence", 6.0), ("turbulence_size", 1.5), ("speed", 7.0)] {
        metadata.params.iter_mut().find(|param| param.id == id).unwrap().default_value = value;
        for binding in metadata.bindings.iter_mut().filter(|binding| binding.id == id) {
            binding.default_value = value;
        }
    }
    let scene = NodeId::new("scene");
    let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        &owner,
        &wind,
        NodeId::new("wind"),
        scene,
        SceneTargetSelection::AllObjects,
    )
    .expect("the Wind card prepares on every object");
    let def = manifold_nodes_scene::node_graph::scene_modifier_authoring::attach_scene_modifier(&owner, instance).expect("the Wind card attaches");
    serde_json::to_string(&def).unwrap()
}

fn build(json: &str) -> PresetRuntime {
    PresetRuntime::from_json_str(json, &registry()).expect("a Wind with turbulence builds over solids, a liquid and a swarm")
}

fn frame(runtime: &mut PresetRuntime, fps: u32, frame: u32, export: bool) {
    let seconds = f64::from(frame) / f64::from(fps);
    let interval = Seconds(1.0 / 60.0);
    runtime.set_sim_step(if export { SimStep::export(interval) } else { SimStep::live(interval) });
    runtime.execute_frame(FrameTime {
        seconds: Seconds(seconds),
        beats: Beats(seconds * 2.0),
        delta: Seconds(if frame == 0 { 0.0 } else { 1.0 / f64::from(fps) }),
        frame_count: i64::from(frame),
    });
}

/// Half a second at `fps`, optionally rebuilt compatibly halfway.
fn play(json: &str, fps: u32, export: bool, rebuild: bool) -> Vec<Reading> {
    READINGS.with_borrow_mut(Vec::clear);
    let mut runtime = build(json);
    for index in 0..=fps / 2 {
        if rebuild && index == fps / 4 {
            let mut rebuilt = build(json);
            rebuilt.carry_generator_state_from(&mut runtime);
            runtime = rebuilt;
        }
        frame(&mut runtime, fps, index, export);
    }
    READINGS.with_borrow_mut(std::mem::take)
}

#[test]
fn wind_gusts_drift_the_same_for_every_consumer_at_any_rate_live_and_export() {
    let json = windy_scene();
    let runtime = build(&json);
    let mask = manifold_nodes_water::runtime::testkit::sampling_mask(&runtime).expect("the consumers replay their ticks");
    let replayed = |kind: &str| {
        runtime.plan.steps().iter().zip(mask).any(|(step, &on)| on && runtime.graph.get_node(step.node).unwrap().node.type_id().as_str() == kind)
    };
    assert!(replayed("node.turbulence_vector_field"), "the gusts replay with the ticks");
    assert!(replayed("node.physics_world"), "the solids share the wind");

    let baseline = play(&json, 120, false, false);
    let ticks = |name: &str| baseline.iter().filter(|reading| reading.0 == name).count();
    assert_eq!((ticks("liquid"), ticks("swarm")), (60, 45), "one reading per tick of each consumer");

    // Where both consumers tick, they read the same gust.
    let mut shared = 0;
    for liquid in baseline.iter().filter(|reading| reading.0 == "liquid") {
        if let Some(swarm) = baseline.iter().find(|reading| reading.0 == "swarm" && reading.1 == liquid.1) {
            assert_eq!(swarm.2, liquid.2, "the drift at tick {}", f64::from_bits(liquid.1));
            shared += 1;
        }
    }
    assert_eq!(shared, 15, "every thirtieth of a second is a tick of both");

    // The gusts move from tick to tick.
    let liquid: Vec<_> = baseline.iter().filter(|reading| reading.0 == "liquid").map(|reading| reading.2).collect();
    assert!(liquid.windows(2).all(|pair| pair[0] != pair[1]), "the gust drifts every tick");

    for (fps, export, rebuild) in [(144, false, false), (144, true, false), (30, false, false), (30, true, false), (120, true, false), (120, false, true), (30, true, true)] {
        let mut run = play(&json, fps, export, rebuild);
        let mut expected = baseline.clone();
        // Readings are compared per consumer in tick order.
        run.sort_by_key(|reading| (reading.0, reading.1));
        expected.sort_by_key(|reading| (reading.0, reading.1));
        assert_eq!(run, expected, "{fps} fps export={export} rebuilt={rebuild}");
    }
}
