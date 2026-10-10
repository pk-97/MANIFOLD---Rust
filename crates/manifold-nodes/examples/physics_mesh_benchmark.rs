//! Bounded measurement of imported objects through the production RigidSimulation.
//! Usage: physics_mesh_benchmark model.glb [pieces=1]
use manifold_core::Seconds;
use {manifold_water_rigid::physics::MAX_BODIES, manifold_water_rigid::physics::RigidBody, manifold_water_rigid::physics::RigidSimulation, manifold_node_engine::scene::mesh_selection::MeshSelection, manifold_water_rigid::physics_mesh::prepare_colliders, manifold_node_engine::scene::mesh_selection::select_fragment, manifold_node_engine::scene::transform::Transform};
use std::{path::Path, sync::Arc, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or("Usage: physics_mesh_benchmark model.glb [pieces]")?;
    let count: usize = args.get(1).map(|v| v.parse()).transpose()?.unwrap_or(1);
    if count == 0 || count >= MAX_BODIES {
        return Err("Use 1–63 pieces".into());
    }
    let start = Instant::now();
    let selection = MeshSelection {
        mesh: -1,
        primitive: -1,
        material: -1,
        fit: true,
        recenter: true,
        translate: [0.0; 3],
        fragment_count: 1,
        fragment_index: 0,
        collider_parts: 32,
    };
    let vertices = selection.load(Path::new(path))?;
    let mut bodies = std::array::from_fn(|_| None);
    bodies[0] = Some(RigidBody {
        kind: 0,
        transform: Transform {
            pos: [0.0, -0.11547005, 0.0],
            scale: [20.0, 0.2, 20.0],
            ..Transform::default()
        },
        ..RigidBody::default()
    });
    for index in 0..count {
        let part = select_fragment(vertices.clone(), count as u32, index as u32)?;
        let collider = Arc::new(prepare_colliders(&part, if count == 1 { 32 } else { 1 })?);
        bodies[index + 1] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 2.1, 0.0],
                scale: [2.2; 3],
                rot_euler: [0.0, 0.0, 0.35],
                ..Transform::default()
            },
            collider: Some(collider),
            ..RigidBody::default()
        });
    }
    let preparation = start.elapsed();
    let mut sim = RigidSimulation::default();
    let init = Instant::now();
    sim.advance(bodies.clone(), [0.0, -9.81, 0.0], Seconds::ZERO, 1.0, 0.0)?;
    let initialization = init.elapsed();
    let mut samples = Vec::with_capacity(360);
    for frame in 1..=360 {
        let start = Instant::now();
        sim.advance(
            bodies.clone(),
            [0.0, -9.81, 0.0],
            Seconds(f64::from(frame) / 60.0),
            1.0,
            0.0,
        )?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let sum: f64 = samples.iter().sum();
    samples.sort_by(f64::total_cmp);
    println!(
        "pieces={count} triangles={} prep={:.3}s init={:.3}ms six_seconds={:.3}s mean={:.3}ms p95={:.3}ms max={:.3}ms lag={:.6}s",
        vertices.len() / 3,
        preparation.as_secs_f64(),
        initialization.as_secs_f64() * 1000.0,
        sum / 1000.0,
        sum / 360.0,
        samples[342],
        samples[359],
        sim.pending_time.0
    );
    assert!(
        sim.poses[..count + 1]
            .iter()
            .all(|p| p.pos.iter().all(|v| v.is_finite())),
        "Nonfinite physics pose"
    );
    Ok(())
}
