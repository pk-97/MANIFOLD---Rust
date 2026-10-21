//! The shipped swarm holds in frame for a long clip at stock World gravity
//! (D-109 with D-114): its CPU twin, read from the preset's own card values.
//! Each tick lays the felt field on the swarm's force lattice, as
//! `node.swarm_forces` does, and steps every particle as `node.move_swarm`
//! does: exact relaxation toward the wind, then x += v·dt.
//!
//! The forces are the stock recipes composed in `FieldValue`: Attractor is
//! the inward radial field times Strength, Radial Force the outward one;
//! Wind is its direction at Speed plus turbulence drifting at Speed times
//! Turbulence, as air at Air Resistance.

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_physics::clock::TICK;
use manifold_physics::force_lattice::ForceLattice;
use manifold_physics::{FalloffCurve, FieldValue, TurbulenceDrift, TurbulenceField, VectorField};

const JSON: &str = include_str!("../assets/generator-presets/Swarm.json");

/// A radial force's card: centre, radius, falloff power, curve and
/// strength (negative pulls inward).
#[derive(Clone, Copy, Debug)]
struct Radial {
    center: [f32; 3],
    radius: f32,
    falloff: f32,
    curve: f32,
    strength: f32,
}

/// The swarm's physics as its card sets it.
#[derive(Clone, Copy, Debug)]
struct Setup {
    gravity: [f32; 3],
    air_drag: f32,
    attractor: Radial,
    core: Radial,
    wind_direction: [f32; 3],
    wind_speed: f32,
    turbulence: f32,
    turbulence_size: f32,
    wind_air_resistance: f32,
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

fn radial(def: &EffectGraphDef, preset: &str, inward: bool) -> Radial {
    let value = |param| force_value(def, preset, param);
    Radial {
        center: ["center_x", "center_y", "center_z"].map(value),
        radius: value("radius"),
        falloff: value("falloff"),
        curve: value("curve"),
        strength: if inward { -value("strength") } else { value("strength") },
    }
}

fn shipped() -> Setup {
    let def: EffectGraphDef = serde_json::from_str(JSON).unwrap();
    let nodes = all_nodes(&def.nodes);
    let bounds = nodes.iter().find(|n| n.node_id.as_str().starts_with("swarm_bounds")).expect("the swarm's box");
    let bounds = |param: &str, default: f32| bounds.params.get(param).map_or(default, number);
    Setup {
        gravity: [world_control(&def, "gravity_x"), world_control(&def, "gravity_y"), world_control(&def, "gravity_z")],
        air_drag: card_value(&def, "node.swarm_forces", "air_drag"),
        attractor: radial(&def, "Attractor", true),
        core: radial(&def, "RadialForce", false),
        wind_direction: ["direction_x", "direction_y", "direction_z"].map(|p| force_value(&def, "Wind", p)),
        wind_speed: force_value(&def, "Wind", "speed"),
        turbulence: force_value(&def, "Wind", "turbulence"),
        turbulence_size: force_value(&def, "Wind", "turbulence_size"),
        wind_air_resistance: force_value(&def, "Wind", "air_resistance"),
        bounds_center: ["pos_x", "pos_y", "pos_z"].map(|p| bounds(p, 0.0)),
        bounds_size: ["scale_x", "scale_y", "scale_z"].map(|p| bounds(p, 1.0)),
        force_detail: card_value(&def, "node.swarm_forces", "force_detail") as u32,
        start_size: nodes.iter().find(|n| n.type_id == "node.cube_mesh").unwrap().params.get("size").map_or(1.0, number),
    }
}

fn radial_field(force: &Radial) -> FieldValue {
    FieldValue::radial_curve(force.center, force.radius, force.falloff, FalloffCurve::from_index(force.curve))
        .and_then(|field| field.scaled(force.strength))
        .unwrap()
}

/// The field the swarm feels at transport time `t` under `drift`.
fn felt(setup: &Setup, t: f64, drift: &mut TurbulenceDrift) -> FieldValue {
    let length = setup.wind_direction.iter().map(|c| c * c).sum::<f32>().sqrt();
    let direction = setup.wind_direction.map(|c| c / length);
    let offset = drift.advance(t, direction.map(|c| f64::from(c * setup.wind_speed)));
    let gusts = FieldValue::turbulence(setup.turbulence_size, TurbulenceField::wrap_offset(setup.turbulence_size, offset), 0)
        .and_then(|field| field.scaled(setup.turbulence))
        .unwrap();
    let wind = FieldValue::uniform(direction.map(|c| c * setup.wind_speed))
        .and_then(|field| field.sum(&gusts))
        .and_then(|field| field.air(setup.wind_air_resistance))
        .unwrap();
    let forces = radial_field(&setup.attractor).sum(&radial_field(&setup.core)).and_then(|field| field.sum(&wind)).unwrap();
    FieldValue::felt_in_still_air(Some(&forces), 0.0, setup.air_drag)
        .unwrap()
        .unwrap()
        .sum(&FieldValue::uniform(setup.gravity).unwrap())
        .unwrap()
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

/// Run `seconds` of the swarm at `fps`: each frame holds its ticks of 1/60 s,
/// and each tick reads the field at its own time.
fn hold(setup: &Setup, fps: f64, seconds: f64) -> Hold {
    let min = std::array::from_fn(|i| setup.bounds_center[i] - 0.5 * setup.bounds_size[i]);
    let lattice = ForceLattice::over_box(min, setup.bounds_size, setup.force_detail);
    let mut values = vec![[0.0f32; 4]; 2 * lattice.node_count()];
    let mut positions = seeds(setup.start_size, 2000);
    let mut velocities = vec![[0.0f32; 3]; positions.len()];
    let mut drift = TurbulenceDrift::default();
    let mut result = Hold { centre_reach: 0.0, lowest: f32::MAX, widest: 0.0, spread: (f32::MAX, 0.0) };
    let mut tick = 0u64;
    for frame in 1..=(seconds * fps).round() as u64 {
        let until = frame as f64 / fps;
        while (tick + 1) as f64 * TICK <= until + 1e-9 {
            let field = felt(setup, tick as f64 * TICK, &mut drift);
            lattice.fill_forces(&mut values, |x| field.sample_force(x)).unwrap();
            for (x, v) in positions.iter_mut().zip(&mut velocities) {
                *v = lattice.sample_forces(&values, *x).step_velocity(*v, TICK as f32);
                *x = std::array::from_fn(|i| x[i] + v[i] * TICK as f32);
            }
            tick += 1;
        }
        let n = positions.len() as f32;
        let centre: [f32; 3] = std::array::from_fn(|i| positions.iter().map(|p| p[i]).sum::<f32>() / n);
        let reach = (0..3).map(|i| (centre[i] - setup.attractor.center[i]).powi(2)).sum::<f32>().sqrt();
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
    let setup = shipped();
    assert_eq!(setup.gravity, [0.0, -9.81, 0.0], "stock World gravity");
    let runs = [30.0, 60.0].map(|fps| std::thread::spawn(move || (fps, hold(&setup, fps, 60.0))));
    for run in runs {
        let (fps, held) = run.join().unwrap();
        assert!(held.centre_reach < setup.attractor.radius, "{fps} fps: the centre stays within the Attractor's radius: {held:?}");
        assert!(held.lowest > -4.0 && held.widest < 6.0, "{fps} fps: no particle leaves the frame: {held:?}");
        assert!(held.spread.0 > 0.3 && held.spread.1 > 1.5 * held.spread.0, "{fps} fps: the cloud keeps a body that breathes: {held:?}");
    }
}
