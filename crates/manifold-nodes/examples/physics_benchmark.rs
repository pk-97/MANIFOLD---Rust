//! Bounded CPU and preview backlog measurements for the bundled PhysicsBoxes scene,
//! and for a world past the old 64-body cap with several copy groups.
//!
//! Run explicitly in release mode with
//! `cargo run --release -p manifold-nodes --example physics_benchmark`.
//! This measures solver cost rather than asserting a hardware-dependent bound.

use std::collections::BTreeMap;

use manifold_core::Seconds;
use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_water_rigid::physics::{CopySpawn, MAX_BODIES, RigidBody, RigidSceneInputs, RigidSimulation, SimStep};
use manifold_node_engine::scene::transform::Transform;

const JSON: &str = include_str!("../assets/generator-presets/PhysicsBoxes.json");
const FRAME: f64 = 1.0 / 60.0;

fn scalar(nodes: &BTreeMap<u32, &EffectGraphNode>, id: u32, name: &str) -> f32 {
    match nodes[&id].params.get(name).expect("PhysicsBoxes parameter") {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Enum { value } => *value as f32,
        other => panic!("unexpected {name} value: {other:?}"),
    }
}

/// PhysicsBoxes as authored, with its one copy group making `count` boxes.
fn physics_boxes(count: usize) -> RigidSceneInputs {
    let def: EffectGraphDef = serde_json::from_str(JSON).expect("PhysicsBoxes JSON");
    let nodes: BTreeMap<_, _> = def.nodes.iter().map(|node| (node.id, node)).collect();
    let body = |id: u32| RigidBody {
        transform: Transform {
            pos: ["pos_x", "pos_y", "pos_z"].map(|p| scalar(&nodes, id - 1, p)),
            rot_euler: ["rot_x", "rot_y", "rot_z"].map(|p| scalar(&nodes, id - 1, p)),
            scale: ["scale_x", "scale_y", "scale_z"].map(|p| scalar(&nodes, id - 1, p)),
            billboard: false,
        },
        enabled: true,
        shape: scalar(&nodes, id, "shape") as u32,
        kind: scalar(&nodes, id, "motion") as u32,
        density: scalar(&nodes, id, "density"),
        friction: scalar(&nodes, id, "friction"),
        bounce: scalar(&nodes, id, "bounce"),
        collider: None,
        fragment_parent: None,
        release_count: 0.0,
        wall: false,
        spawn: Default::default(),
    };
    let mut scene = RigidSceneInputs { gravity: [0.0, -9.81, 0.0], ..RigidSceneInputs::default() };
    for (slot, id) in [101, 141, 161].into_iter().enumerate() {
        scene.bodies[slot] = Some(body(id));
    }
    scene.groups[0] = Some(RigidBody {
        spawn: CopySpawn {
            count: count as f32,
            spacing: scalar(&nodes, 121, "copy_spacing"),
            columns: scalar(&nodes, 121, "copy_columns"),
            layout: scalar(&nodes, 121, "copy_layout"),
        },
        ..body(121)
    });
    scene
}

/// 500 bodies in one world: a floor, 255 individually wired boxes in a
/// loose stack, and two copy groups of 122 boxes piled on top.
fn wide_world() -> RigidSceneInputs {
    let boxed = |pos: [f32; 3], scale: f32, kind: u32| RigidBody {
        transform: Transform { pos, rot_euler: [0.1, 0.2, 0.05], scale: [scale; 3], billboard: false },
        kind,
        ..RigidBody::default()
    };
    let mut bodies = vec![None; MAX_BODIES];
    bodies[0] = Some(RigidBody {
        transform: Transform { pos: [0.0, -0.5, 0.0], scale: [60.0, 1.0, 60.0], ..Transform::default() },
        kind: 0,
        ..RigidBody::default()
    });
    for (slot, body) in bodies.iter_mut().enumerate().skip(1) {
        let i = slot - 1;
        let pos = [(i % 8) as f32 * 1.4 - 5.0, 1.0 + (i / 64) as f32 * 1.4, ((i / 8) % 8) as f32 * 1.4 - 5.0];
        *body = Some(boxed(pos, 0.5, 1));
    }
    let pile = |x: f32| RigidBody {
        spawn: CopySpawn { count: 122.0, spacing: 1.2, columns: 8.0, layout: 1.0 },
        ..boxed([x, 8.0, 0.0], 0.45, 1)
    };
    RigidSceneInputs {
        groups: vec![Some(pile(-3.0)), Some(pile(3.0))],
        gravity: [0.0, -9.81, 0.0],
        targeted_fields: vec![None; MAX_BODIES + 2],
        bodies,
        acceleration_field: None,
    }
}

fn advance(sim: &mut RigidSimulation, scene: &RigidSceneInputs, frame: f64) {
    sim.advance_scene(scene, Seconds(frame), 1.0, 0.0)
        .expect("benchmark simulation must advance");
}

fn print_stats(label: &str, values: &[f32]) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mean = values.iter().map(|value| f64::from(*value)).sum::<f64>() / values.len() as f64;
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[((sorted.len() - 1) * 95) / 100];
    let max = sorted[sorted.len() - 1];
    eprintln!(
        "{label}: samples={} physics_ms mean={mean:.3} median={median:.3} p95={p95:.3} max={max:.3}",
        values.len()
    );
}

fn steady_samples(label: &str, scene: &RigidSceneInputs) {
    let mut sim = RigidSimulation::default().with_step(SimStep::export(manifold_core::Seconds(FRAME)));
    for frame in 0..=8 {
        advance(&mut sim, scene, frame as f64 * FRAME);
    }
    let mut samples = Vec::with_capacity(8);
    for frame in 9..=16 {
        advance(&mut sim, scene, frame as f64 * FRAME);
        samples.push(sim.physics_ms);
    }
    let bodies = scene.bodies.iter().flatten().count() + sim.total_active_copies();
    print_stats(&format!("{label} bodies={bodies}"), &samples);
}

fn hitch_lag(label: &str, scene: &RigidSceneInputs) {
    let mut sim = RigidSimulation::default().with_step(SimStep::live(manifold_core::Seconds(FRAME)));
    advance(&mut sim, scene, 0.0);
    advance(&mut sim, scene, 1.0);
    eprintln!(
        "{label}: after 1.000 s hitch preview physics_ms={:.3} pending={:.3} s",
        sim.physics_ms, sim.pending_time.0
    );
}

fn main() {
    eprintln!("Physics benchmark: 8 warmup + 8 measured ticks; hitch budget=16.667 ms");
    for count in [256, 500, 4_000] {
        let scene = physics_boxes(count);
        let label = format!("PhysicsBoxes count={count}");
        steady_samples(&label, &scene);
        hitch_lag(&label, &scene);
    }
    let wide = wide_world();
    steady_samples("Wide world 256 wired + 2 copy groups", &wide);
    hitch_lag("Wide world 256 wired + 2 copy groups", &wide);
}
