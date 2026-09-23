//! Bounded CPU and preview backlog measurements for the bundled PhysicsBoxes scene.
//!
//! Run explicitly in release mode with
//! `cargo run --release -p manifold-renderer --example physics_benchmark`.
//! This measures solver cost rather than asserting a hardware-dependent bound.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::Seconds;
use manifold_renderer::node_graph::physics::{PhysicsStepScope, RigidBody, RigidSimulation};
use manifold_renderer::node_graph::transform::Transform;

const JSON: &str = include_str!("../assets/generator-presets/PhysicsBoxes.json");
const FRAME: f64 = 1.0 / 60.0;

fn scalar(nodes: &BTreeMap<u32, &EffectGraphNode>, id: u32, name: &str) -> f32 {
    match nodes[&id].params.get(name).expect("PhysicsBoxes parameter") {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Enum { value } => *value as f32,
        other => panic!("unexpected {name} value: {other:?}"),
    }
}

fn scene_inputs() -> ([Option<RigidBody>; 16], RigidBody, f32, f32, f32, f32) {
    let def: EffectGraphDef = serde_json::from_str(JSON).expect("PhysicsBoxes JSON");
    let nodes: BTreeMap<_, _> = def.nodes.iter().map(|node| (node.id, node)).collect();
    let body = |id: u32| RigidBody {
        transform: Transform {
            pos: ["pos_x", "pos_y", "pos_z"].map(|p| scalar(&nodes, id - 1, p)),
            rot_euler: ["rot_x", "rot_y", "rot_z"].map(|p| scalar(&nodes, id - 1, p)),
            scale: ["scale_x", "scale_y", "scale_z"].map(|p| scalar(&nodes, id - 1, p)),
            billboard: false,
        },
        shape: scalar(&nodes, id, "shape") as u32,
        kind: scalar(&nodes, id, "motion") as u32,
        mass: scalar(&nodes, id, "mass"),
        friction: scalar(&nodes, id, "friction"),
        bounce: scalar(&nodes, id, "bounce"),
    };
    let mut bodies = [None; 16];
    for (slot, id) in [101, 141, 161].into_iter().enumerate() {
        bodies[slot] = Some(body(id));
    }
    (
        bodies,
        body(121),
        scalar(&nodes, 40, "copy_spacing"),
        scalar(&nodes, 40, "copy_columns"),
        scalar(&nodes, 40, "copy_layout"),
        -9.81,
    )
}

fn advance(
    sim: &mut RigidSimulation,
    bodies: [Option<RigidBody>; 16],
    prototype: RigidBody,
    count: f32,
    spacing: f32,
    columns: f32,
    layout: f32,
    gravity_y: f32,
    frame: f64,
    reset: f32,
) {
    sim.advance_with_copy_layout(
        bodies,
        Some(prototype),
        count,
        spacing,
        columns,
        layout,
        [0.0, gravity_y, 0.0],
        Seconds(frame),
        1.0,
        reset,
    )
    .expect("PhysicsBoxes simulation must advance");
}

fn print_stats(count: usize, values: &[f32]) {
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let mean = values.iter().map(|value| f64::from(*value)).sum::<f64>() / values.len() as f64;
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[((sorted.len() - 1) * 95) / 100];
    let max = sorted[sorted.len() - 1];
    eprintln!(
        "PhysicsBoxes count={count}: samples={} physics_ms mean={mean:.3} median={median:.3} p95={p95:.3} max={max:.3}",
        values.len()
    );
}

fn steady_samples(count: usize) {
    let (bodies, prototype, spacing, columns, layout, gravity_y) = scene_inputs();
    let mut sim = RigidSimulation::default();
    let _scope = PhysicsStepScope::with_preview_budget(true, std::time::Duration::ZERO);
    advance(
        &mut sim,
        bodies,
        prototype,
        count as f32,
        spacing,
        columns,
        layout,
        gravity_y,
        0.0,
        0.0,
    );
    for frame in 1..=8 {
        advance(
            &mut sim,
            bodies,
            prototype,
            count as f32,
            spacing,
            columns,
            layout,
            gravity_y,
            frame as f64 * FRAME,
            0.0,
        );
    }
    let mut samples = Vec::with_capacity(8);
    for frame in 9..=16 {
        advance(
            &mut sim,
            bodies,
            prototype,
            count as f32,
            spacing,
            columns,
            layout,
            gravity_y,
            frame as f64 * FRAME,
            0.0,
        );
        samples.push(sim.physics_ms);
    }
    print_stats(count, &samples);
}

fn hitch_lag(count: usize) {
    let (bodies, prototype, spacing, columns, layout, gravity_y) = scene_inputs();
    let mut sim = RigidSimulation::default();
    let _scope = PhysicsStepScope::for_render(false);
    advance(
        &mut sim,
        bodies,
        prototype,
        count as f32,
        spacing,
        columns,
        layout,
        gravity_y,
        0.0,
        0.0,
    );
    advance(
        &mut sim,
        bodies,
        prototype,
        count as f32,
        spacing,
        columns,
        layout,
        gravity_y,
        1.0,
        0.0,
    );
    eprintln!(
        "PhysicsBoxes count={count}: after 1.000 s hitch preview physics_ms={:.3} pending={:.3} s",
        sim.physics_ms, sim.pending_time.0
    );
}

fn main() {
    eprintln!("PhysicsBoxes benchmark: 8 warmup + 8 measured ticks; hitch budget=16.667 ms");
    for count in [256, 4_000] {
        steady_samples(count);
        hitch_lag(count);
    }
}
