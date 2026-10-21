//! The shipped swarm holds in frame for a long clip at stock World gravity
//! (D-109 with D-114). The forces are what ships: Swarm.json's Attractor,
//! Radial Force and Wind recipes expanded into the scene graph, and the
//! field feeding `node.swarm_forces` evaluated on the CPU at every tick's
//! time with the card's values. Each tick the swarm feels that field
//! against still air at its Air Drag plus World gravity, as
//! `node.swarm_forces` composes it, laid on its force lattice; each particle
//! steps as `node.move_swarm` does: exact relaxation toward the wind, then
//! x += v·dt.

use std::sync::mpsc::{Receiver, Sender, channel};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::effects::ParamConvert;
use manifold_core::{Beats, Seconds};
use manifold_node_engine::exec::effect_node::{EffectNode, EffectNodeContext, EffectNodeType, FrameTime};
use manifold_node_engine::exec::execution::Executor;
use manifold_node_engine::exec::execution_plan::{ExecutionPlan, compile};
use manifold_node_engine::graph::Graph;
use manifold_node_engine::load::expand::prepare_scene_modifiers;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_physics::clock::TICK;
use manifold_physics::force_lattice::ForceLattice;
use manifold_physics::{FieldValue, VectorField};

const JSON: &str = include_str!("../assets/generator-presets/Swarm.json");

/// The swarm's physics outside its forces, as its card sets it.
#[derive(Clone, Copy, Debug)]
struct Setup {
    gravity: [f32; 3],
    world_air_resistance: f32,
    air_drag: f32,
    attractor_center: [f32; 3],
    attractor_radius: f32,
    bounds_center: [f32; 3],
    bounds_size: [f32; 3],
    force_detail: u32,
    start_size: f32,
}

fn all_nodes(nodes: &[EffectGraphNode]) -> Vec<&EffectGraphNode> {
    nodes.iter().flat_map(|node| std::iter::once(node).chain(node.group.as_deref().map_or(Vec::new(), |body| all_nodes(&body.nodes)))).collect()
}

fn number(value: &SerializedParamValue) -> f32 {
    match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        SerializedParamValue::Enum { value } => *value as f32,
        other => panic!("not a number: {other:?}"),
    }
}

/// The value the card gives `param` of the one node of `type_id`, else its
/// authored param.
fn card_value(def: &EffectGraphDef, type_id: &str, param: &str) -> f32 {
    let nodes = all_nodes(&def.nodes);
    let node = nodes.iter().find(|node| node.type_id == type_id).unwrap_or_else(|| panic!("a {type_id}"));
    let meta = def.preset_metadata.as_ref().unwrap();
    meta.bindings
        .iter()
        .find(|binding| matches!(&binding.target, BindingTarget::Node { node_id, param: p } if *node_id == node.node_id && p == param))
        .map(|binding| binding.default_value)
        .or_else(|| node.params.get(param).map(number))
        .unwrap_or_else(|| panic!("{type_id}.{param} is set"))
}

/// The value a Physics World control's source holds.
fn world_control(def: &EffectGraphDef, port: &str) -> f32 {
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").expect("a World");
    let wire = def.wires.iter().find(|wire| wire.to_node == world.id && wire.to_port == port).expect("a World source");
    number(def.nodes.iter().find(|node| node.id == wire.from_node).unwrap().params.get("value").unwrap())
}

/// The card default of `param` on the force from the stock preset `preset`.
fn force_value(def: &EffectGraphDef, preset: &str, param: &str) -> f32 {
    let force = def
        .scene_modifiers
        .iter()
        .find(|instance| instance.graph.preset_metadata.as_ref().unwrap().id.as_str() == preset)
        .unwrap_or_else(|| panic!("the swarm brings a {preset}"));
    force.graph.preset_metadata.as_ref().unwrap().params.iter().find(|spec| spec.id == param).unwrap().default_value
}

fn shipped(def: &EffectGraphDef) -> Setup {
    let nodes = all_nodes(&def.nodes);
    let bounds = nodes.iter().find(|n| n.node_id.as_str().starts_with("swarm_bounds")).expect("the swarm's box");
    let bounds = |param: &str, default: f32| bounds.params.get(param).map_or(default, number);
    Setup {
        gravity: [world_control(def, "gravity_x"), world_control(def, "gravity_y"), world_control(def, "gravity_z")],
        world_air_resistance: world_control(def, "air_resistance"),
        air_drag: card_value(def, "node.swarm_forces", "air_drag"),
        attractor_center: ["center_x", "center_y", "center_z"].map(|p| force_value(def, "Attractor", p)),
        attractor_radius: force_value(def, "Attractor", "radius"),
        bounds_center: ["pos_x", "pos_y", "pos_z"].map(|p| bounds(p, 0.0)),
        bounds_size: ["scale_x", "scale_y", "scale_z"].map(|p| bounds(p, 1.0)),
        force_detail: card_value(def, "node.swarm_forces", "force_detail") as u32,
        start_size: nodes.iter().find(|n| n.type_id == "node.cube_mesh").unwrap().params.get("size").map_or(1.0, number),
    }
}

/// Hands each frame's field out of the graph.
struct FieldTap {
    type_id: EffectNodeType,
    out: Sender<FieldValue>,
}

impl EffectNode for FieldTap {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("field"),
            ty: PortType::VectorField,
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        static OUTPUTS: [NodeOutput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("seen"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Output,
            required: false,
        }];
        &OUTPUTS
    }

    fn parameters(&self) -> &[manifold_node_engine::parameters::ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(field) = ctx.inputs.cpu_value::<FieldValue>("field") else {
            ctx.mark_outputs_pending();
            return;
        };
        self.out.send(field).expect("the test reads every field");
        ctx.outputs.set_scalar("seen", ParamValue::Float(1.0));
    }
}

struct ScalarSink {
    type_id: EffectNodeType,
}

impl EffectNode for ScalarSink {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn inputs(&self) -> &[NodeInput] {
        static INPUTS: [NodeInput; 1] = [NodePort {
            name: std::borrow::Cow::Borrowed("seen"),
            ty: PortType::Scalar(ScalarType::F32),
            kind: PortKind::Input,
            required: true,
        }];
        &INPUTS
    }

    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }

    fn parameters(&self) -> &[manifold_node_engine::parameters::ParamDef] {
        &[]
    }

    fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
}

/// The shipped forces as a CPU graph: Swarm.json with its force recipes
/// expanded, cut to the ancestry of the field `node.swarm_forces` takes, its
/// nodes at the card's values, and a tap on that field.
struct Forces {
    graph: Graph,
    plan: ExecutionPlan,
    executor: Executor,
    fields: Receiver<FieldValue>,
}

impl Forces {
    fn shipped(def: &EffectGraphDef) -> Self {
        let registry = PrimitiveRegistry::with_builtin();
        let expanded = prepare_scene_modifiers(def, &registry).expect("the swarm's forces expand").def;
        let mut flat = manifold_core::flatten::flatten_groups(&expanded).expect("flattens");
        let swarm = flat.nodes.iter().find(|node| node.type_id == "node.swarm_forces").expect("the swarm's forces node").id;
        let into = flat
            .wires
            .iter()
            .find(|wire| wire.to_node == swarm && wire.to_port == "acceleration_field")
            .expect("expansion wires the forces into the swarm")
            .clone();
        let mut keep = std::collections::BTreeSet::new();
        let mut pending = vec![into.from_node];
        while let Some(id) = pending.pop() {
            if keep.insert(id) {
                pending.extend(flat.wires.iter().filter(|wire| wire.to_node == id).map(|wire| wire.from_node));
            }
        }
        flat.nodes.retain(|node| keep.contains(&node.id));
        flat.wires.retain(|wire| keep.contains(&wire.to_node) && keep.contains(&wire.from_node));
        // The card's values reach the nodes they bind, as the host binds them.
        let bindings = flat.preset_metadata.take().map(|meta| meta.bindings).unwrap_or_default();
        for binding in bindings.iter().filter(|binding| binding.convert == ParamConvert::Float) {
            let BindingTarget::Node { node_id, param } = &binding.target else { continue };
            if let Some(node) = flat.nodes.iter_mut().find(|node| node.node_id == *node_id)
                && let Some(SerializedParamValue::Float { value }) = node.params.get_mut(param)
            {
                *value = binding.default_value * binding.scale + binding.offset;
            }
        }
        let producer = flat.nodes.iter().find(|node| node.id == into.from_node).unwrap().node_id.clone();
        let mut graph = flat.into_graph(&registry, &Default::default()).expect("the forces build");
        let producer = graph.instance_by_node_id(&producer).expect("the field's producer");
        let (out, fields) = channel();
        let tap = graph.add_node(Box::new(FieldTap { type_id: EffectNodeType::new("test.swarm_field_tap"), out }));
        let sink = graph.add_node(Box::new(ScalarSink { type_id: EffectNodeType::new("test.swarm_field_sink") }));
        assert_eq!(into.from_port, "out", "the forces leave by their field output");
        graph.connect((producer, "out"), (tap, "field")).unwrap();
        graph.connect((tap, "seen"), (sink, "seen")).unwrap();
        let plan = compile(&graph).expect("the forces compile");
        let mut executor = Executor::with_mock();
        executor.set_preview_target(Some(tap));
        Self { graph, plan, executor, fields }
    }

    /// The scene's forces at transport time `t`.
    fn at(&mut self, t: f64) -> FieldValue {
        let time = FrameTime { beats: Beats(0.0), seconds: Seconds(t), delta: Seconds(TICK), frame_count: 0 };
        self.executor.execute_frame(&mut self.graph, &self.plan, time);
        self.fields.try_recv().expect("the forces evaluate every tick")
    }
}

/// Particles seeded on the start cube's surface, deterministically.
fn seeds(size: f32, count: usize) -> Vec<[f32; 3]> {
    let mut state = 0x9E37_79B9_u32;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / u32::MAX as f32
    };
    (0..count)
        .map(|i| {
            let (u, v) = (next() - 0.5, next() - 0.5);
            let side = if i % 2 == 0 { 0.5 } else { -0.5 };
            let p = match i % 3 {
                0 => [side, u, v],
                1 => [u, side, v],
                _ => [u, v, side],
            };
            p.map(|c| c * size)
        })
        .collect()
}

/// What the cloud did over a clip.
#[derive(Clone, Copy, Debug)]
struct Hold {
    /// Farthest the cloud's centre got from the Attractor's centre.
    centre_reach: f32,
    /// Lowest any particle got, and farthest sideways.
    lowest: f32,
    widest: f32,
    /// The cloud's smallest and largest rms radius once settled (after 3 s).
    spread: (f32, f32),
}

/// Run `seconds` of the shipped swarm at `fps`: each frame holds its ticks
/// of 1/60 s, and each tick reads the forces at its own time.
fn hold(def: &EffectGraphDef, fps: f64, seconds: f64) -> Hold {
    let setup = shipped(def);
    let mut forces = Forces::shipped(def);
    let gravity = FieldValue::uniform(setup.gravity).unwrap();
    let min = std::array::from_fn(|i| setup.bounds_center[i] - 0.5 * setup.bounds_size[i]);
    let lattice = ForceLattice::over_box(min, setup.bounds_size, setup.force_detail);
    let mut values = vec![[0.0f32; 4]; 2 * lattice.node_count()];
    let mut positions = seeds(setup.start_size, 2000);
    let mut velocities = vec![[0.0f32; 3]; positions.len()];
    let mut result = Hold { centre_reach: 0.0, lowest: f32::MAX, widest: 0.0, spread: (f32::MAX, 0.0) };
    let mut tick = 0u64;
    for frame in 1..=(seconds * fps).round() as u64 {
        let until = frame as f64 / fps;
        while (tick + 1) as f64 * TICK <= until + 1e-9 {
            let scene = forces.at(tick as f64 * TICK);
            let field = FieldValue::felt_in_still_air(Some(&scene), setup.world_air_resistance, setup.air_drag)
                .unwrap()
                .unwrap()
                .sum(&gravity)
                .unwrap();
            lattice.fill_forces(&mut values, |x| field.sample_force(x)).unwrap();
            for (x, v) in positions.iter_mut().zip(&mut velocities) {
                *v = lattice.sample_forces(&values, *x).step_velocity(*v, TICK as f32);
                *x = std::array::from_fn(|i| x[i] + v[i] * TICK as f32);
            }
            tick += 1;
        }
        let n = positions.len() as f32;
        let centre: [f32; 3] = std::array::from_fn(|i| positions.iter().map(|p| p[i]).sum::<f32>() / n);
        let reach = (0..3).map(|i| (centre[i] - setup.attractor_center[i]).powi(2)).sum::<f32>().sqrt();
        result.centre_reach = result.centre_reach.max(reach);
        for p in &positions {
            result.lowest = result.lowest.min(p[1]);
            result.widest = result.widest.max(p[0].abs()).max(p[2].abs());
        }
        if until >= 3.0 {
            let rms = (positions.iter().map(|p| (0..3).map(|i| (p[i] - centre[i]).powi(2)).sum::<f32>()).sum::<f32>() / n).sqrt();
            result.spread = (result.spread.0.min(rms), result.spread.1.max(rms));
        }
    }
    result
}

/// At stock World gravity with no still air, the shipped swarm holds for a
/// 60 s clip at 30 and 60 fps: its centre never leaves the Attractor's
/// radius, no particle leaves the frame (about 4.5 m below the swarm's
/// centre and 10 m to each side), and the cloud keeps a body that breathes
/// rather than collapsing to a point.
#[test]
fn swarm_holds_in_frame_for_a_minute_at_stock_gravity() {
    let def: EffectGraphDef = serde_json::from_str(JSON).unwrap();
    let setup = shipped(&def);
    assert_eq!(setup.gravity, [0.0, -9.81, 0.0], "stock World gravity");
    assert_eq!(setup.world_air_resistance, 0.0, "no still air");
    let runs = [30.0, 60.0].map(|fps| {
        let def = def.clone();
        std::thread::spawn(move || (fps, hold(&def, fps, 60.0)))
    });
    for run in runs {
        let (fps, held) = run.join().unwrap();
        assert!(held.centre_reach < setup.attractor_radius, "{fps} fps: the centre stays within the Attractor's radius: {held:?}");
        assert!(held.lowest > -4.0 && held.widest < 6.0, "{fps} fps: no particle leaves the frame: {held:?}");
        assert!(held.spread.0 > 0.3 && held.spread.1 > 1.5 * held.spread.0, "{fps} fps: the cloud keeps a body that breathes: {held:?}");
    }
}
