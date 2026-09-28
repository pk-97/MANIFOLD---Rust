use super::DT;

use crate::{
    Bounds, Config, FluidWorld, LiquidOptions, MeshHandle, MeshRole, RigidBodyState, RigidReaction,
    TimeStepOptions,
};
use manifold_physics::{BodyConfig, BodyHandle, BodyKind, PhysicsWorld};

fn box_hull(center: [f32; 3], size: [f32; 3]) -> Vec<[f32; 3]> {
    let half = size.map(|value| value * 0.5);
    let [cx, cy, cz] = center;
    let [hx, hy, hz] = half;
    vec![
        [cx - hx, cy - hy, cz - hz],
        [cx + hx, cy - hy, cz - hz],
        [cx + hx, cy + hy, cz - hz],
        [cx - hx, cy + hy, cz - hz],
        [cx - hx, cy - hy, cz + hz],
        [cx + hx, cy - hy, cz + hz],
        [cx + hx, cy + hy, cz + hz],
        [cx - hx, cy + hy, cz + hz],
    ]
}

fn yaw(angle: f32) -> [f32; 4] {
    [0.0, (angle * 0.5).sin(), 0.0, (angle * 0.5).cos()]
}

fn fluid(viscosity: f64) -> FluidWorld {
    let mut fluid = FluidWorld::new(Config {
        cells: [24; 3],
        cell_size: 0.1,
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    fluid.set_gravity([0.0; 3]).unwrap();
    fluid
        .set_time_step_options(TimeStepOptions {
            min_substeps: 2,
            max_substeps: 32,
            cfl: 1,
            adaptive_obstacles: false,
        })
        .unwrap();
    fluid
        .set_liquid_options(LiquidOptions {
            viscosity,
            surface_tension: 0.0,
        })
        .unwrap();
    fluid
}

fn u_hulls() -> Vec<Vec<[f32; 3]>> {
    vec![
        box_hull([0.0, -0.3, 0.0], [0.9, 0.2, 0.6]),
        box_hull([-0.4, 0.0, 0.0], [0.2, 0.6, 0.6]),
        box_hull([0.4, 0.0, 0.0], [0.2, 0.6, 0.6]),
    ]
}

fn compound_pair(
    ratio: f32,
    viscosity: f64,
    kind: BodyKind,
    fill: bool,
) -> (FluidWorld, PhysicsWorld, BodyHandle, Vec<MeshHandle>) {
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let body = rigid
        .add_hulls(
            &u_hulls(),
            BodyConfig {
                kind,
                position: [1.2, 1.05, 1.2],
                rotation: yaw(0.23),
                mass: 252.0 * ratio,
                ..BodyConfig::default()
            },
        )
        .unwrap();

    let mut fluid = fluid(viscosity);
    let pose = rigid.pose(body).unwrap();
    let installed = rigid.hull_meshes(body).unwrap();
    let colliders: Vec<_> = installed
        .iter()
        .map(|mesh| fluid.add_mesh(mesh, MeshRole::Collider, pose).unwrap())
        .collect();
    for &collider in &colliders {
        fluid.set_collider_friction(collider, 1.0).unwrap();
    }
    if fill {
        fluid
            .add_fluid_box(
                Bounds {
                    min: [0.45, 0.45, 0.45],
                    max: [1.95, 1.8, 1.95],
                },
                [0.0; 3],
            )
            .unwrap();
        fluid.step(DT).unwrap();
    }
    (fluid, rigid, body, colliders)
}

fn state(rigid: &PhysicsWorld, body: BodyHandle) -> RigidBodyState {
    RigidBodyState {
        pose: rigid.pose(body).unwrap(),
        dynamics: rigid.dynamics(body).unwrap(),
    }
}

fn assert_response(
    before: manifold_physics::BodyDynamics,
    after: manifold_physics::BodyDynamics,
    reaction: RigidReaction,
) {
    for axis in 0..3 {
        assert!(
            (f64::from(after.linear_velocity[axis] - before.linear_velocity[axis])
                - reaction.delta_linear[axis])
                .abs()
                < 5e-5
        );
        assert!(
            (f64::from(after.angular_velocity[axis] - before.angular_velocity[axis])
                - reaction.delta_angular[axis])
                .abs()
                < 5e-5
        );
    }
}

#[test]
fn production_compound_coupling_exchanges_one_reaction_per_body() {
    for ratio in [0.1, 1.0] {
        for viscosity in [0.0, 1.0] {
            let (mut fluid, mut rigid, body, colliders) =
                compound_pair(ratio, viscosity, BodyKind::Dynamic, true);
            fluid
                .prepare_rigid_coupling_groups(&[colliders.as_slice()], 1000.0)
                .unwrap();
            rigid
                .set_velocity(body, [0.4, 0.0, 0.1], [0.1, 0.25, -0.1])
                .unwrap();

            let mut total_impulse = 0.0;
            for frame_index in 0..3 {
                let mut frame = fluid.begin_frame(DT).unwrap();
                let mut elapsed = 0.0;
                while elapsed < DT.0 - 1e-12 {
                    let before = rigid.dynamics(body).unwrap();
                    frame.set_rigid_bodies(&[state(&rigid, body)]).unwrap();
                    let dt = frame.next_substep().unwrap().unwrap();
                    frame.advance(dt).unwrap_or_else(|error| panic!("ratio={ratio} viscosity={viscosity} frame={frame_index} elapsed={elapsed} dt={dt:?}: {error}"));
                    let reactions = frame.rigid_reactions().unwrap();
                    assert_eq!(reactions.len(), 1);
                    let reaction = reactions[0];
                    total_impulse += reaction
                        .linear
                        .iter()
                        .chain(reaction.angular.iter())
                        .map(|value| value.abs())
                        .sum::<f64>();
                    rigid
                        .apply_impulses(&[reaction.body_impulse(body).unwrap()])
                        .unwrap();
                    let after = rigid.dynamics(body).unwrap();
                    assert_response(before, after, reaction);
                    rigid.step(dt, 1).unwrap();
                    elapsed += dt.0;
                }
                assert_eq!(frame.next_substep().unwrap(), None);
                let stats = frame.finish().unwrap();
                assert!(stats.substeps > 0);
            }
            assert!(total_impulse > 1e-3);
            let pose = rigid.pose(body).unwrap();
            let dynamics = rigid.dynamics(body).unwrap();
            assert!(
                pose.position
                    .iter()
                    .chain(pose.rotation.iter())
                    .all(|value| value.is_finite())
            );
            assert!(
                dynamics
                    .linear_velocity
                    .iter()
                    .chain(dynamics.angular_velocity.iter())
                    .all(|value| value.is_finite())
            );
        }
    }
}

fn cavity_particles(envelope: bool) -> u32 {
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let hulls = if envelope {
        vec![u_hulls().into_iter().flatten().collect()]
    } else {
        u_hulls()
    };
    let body = rigid
        .add_hulls(
            &hulls,
            BodyConfig {
                kind: BodyKind::Fixed,
                position: [1.2, 1.05, 1.2],
                rotation: yaw(0.23),
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut fluid = fluid(0.0);
    let pose = rigid.pose(body).unwrap();
    let colliders: Vec<_> = rigid
        .hull_meshes(body)
        .unwrap()
        .iter()
        .map(|mesh| fluid.add_mesh(mesh, MeshRole::Collider, pose).unwrap())
        .collect();
    fluid
        .add_fluid_box(
            Bounds {
                min: [1.0, 0.9, 1.0],
                max: [1.4, 1.2, 1.4],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.step(DT).unwrap();
    fluid
        .prepare_rigid_coupling_groups(&[&colliders], 1000.0)
        .unwrap();
    let mut particles = 0;
    for _ in 0..2 {
        let mut frame = fluid.begin_frame(DT).unwrap();
        let mut remaining = DT.0;
        while remaining > 0.0 {
            frame.set_rigid_bodies(&[state(&rigid, body)]).unwrap();
            let dt = frame.next_substep().unwrap().unwrap();
            frame.advance(dt).unwrap();
            rigid.step(dt, 1).unwrap();
            remaining -= dt.0;
        }
        particles = frame.finish().unwrap().particles;
    }
    particles
}

#[test]
fn production_compound_cavity_preserves_fluid_against_convex_envelope() {
    let u_particles = cavity_particles(false);
    let envelope_particles = cavity_particles(true);
    println!("compound cavity particles: U={u_particles}, envelope={envelope_particles}");
    assert!(u_particles > 0);
    assert!(u_particles > envelope_particles);
}

#[test]
fn production_compound_prepare_rejects_invalid_groups_and_allows_retry() {
    let (mut fluid, mut rigid, body, colliders) = compound_pair(1.0, 0.0, BodyKind::Fixed, false);
    let empty: [MeshHandle; 0] = [];
    assert!(
        fluid
            .prepare_rigid_coupling_groups(&[empty.as_slice()], 1000.0)
            .is_err()
    );
    let duplicate = [colliders[0], colliders[0]];
    assert!(
        fluid
            .prepare_rigid_coupling_groups(&[duplicate.as_slice()], 1000.0)
            .is_err()
    );
    let first = [colliders[0]];
    let across = [colliders[0]];
    assert!(
        fluid
            .prepare_rigid_coupling_groups(&[first.as_slice(), across.as_slice()], 1000.0)
            .is_err()
    );

    let (mut foreign_fluid, foreign_rigid, foreign_body, _) =
        compound_pair(1.0, 0.0, BodyKind::Fixed, false);
    let foreign_mesh = foreign_rigid.hull_meshes(foreign_body).unwrap().remove(0);
    let foreign_handle = foreign_fluid
        .add_mesh(
            &foreign_mesh,
            MeshRole::Collider,
            foreign_rigid.pose(foreign_body).unwrap(),
        )
        .unwrap();
    assert!(
        fluid
            .prepare_rigid_coupling_groups(&[std::slice::from_ref(&foreign_handle)], 1000.0)
            .is_err()
    );

    fluid
        .prepare_rigid_coupling_groups(&[colliders.as_slice()], 1000.0)
        .unwrap();
    let mut frame = fluid.begin_frame(DT).unwrap();
    let mut remaining = DT.0;
    while remaining > 0.0 {
        frame.set_rigid_bodies(&[state(&rigid, body)]).unwrap();
        let dt = frame.next_substep().unwrap().unwrap();
        frame.advance(dt).unwrap();
        assert_eq!(frame.rigid_reactions().unwrap().len(), 1);
        rigid.step(dt, 1).unwrap();
        remaining -= dt.0;
    }
    frame.finish().unwrap();
}

#[test]
fn production_compound_groups_preserve_reversed_interleaved_order() {
    let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
    let body_a = rigid
        .add_hull(
            &box_hull([0.0, 0.0, 0.0], [0.4, 0.4, 0.4]),
            BodyConfig {
                position: [0.8, 1.0, 1.2],
                mass: 80.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let body_b = rigid
        .add_hulls(
            &[
                box_hull([-0.3, 0.0, 0.0], [0.25, 0.5, 0.5]),
                box_hull([0.3, 0.0, 0.0], [0.25, 0.5, 0.5]),
            ],
            BodyConfig {
                position: [1.6, 1.0, 1.2],
                mass: 120.0,
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let mut fluid = fluid(0.0);
    let pose_a = rigid.pose(body_a).unwrap();
    let pose_b = rigid.pose(body_b).unwrap();
    let meshes_a = rigid.hull_meshes(body_a).unwrap();
    let meshes_b = rigid.hull_meshes(body_b).unwrap();
    let b0 = fluid
        .add_mesh(&meshes_b[0], MeshRole::Collider, pose_b)
        .unwrap();
    let a0 = fluid
        .add_mesh(&meshes_a[0], MeshRole::Collider, pose_a)
        .unwrap();
    let b1 = fluid
        .add_mesh(&meshes_b[1], MeshRole::Collider, pose_b)
        .unwrap();
    fluid
        .add_fluid_box(
            Bounds {
                min: [0.3, 0.5, 0.7],
                max: [2.1, 1.6, 1.7],
            },
            [0.0; 3],
        )
        .unwrap();
    fluid.step(DT).unwrap();

    let group_b = [b0, b1];
    let group_a = [a0];
    fluid
        .prepare_rigid_coupling_groups(&[&group_b, &group_a], 1000.0)
        .unwrap();
    rigid
        .set_velocity(body_a, [0.35, 0.0, 0.0], [0.0, 0.1, 0.0])
        .unwrap();
    rigid
        .set_velocity(body_b, [-0.2, 0.0, 0.15], [0.0, -0.2, 0.0])
        .unwrap();

    let mut frame = fluid.begin_frame(DT).unwrap();
    let mut remaining = DT.0;
    let mut total = [0.0_f64; 2];
    while remaining > 0.0 {
        let before_b = rigid.dynamics(body_b).unwrap();
        let before_a = rigid.dynamics(body_a).unwrap();
        frame
            .set_rigid_bodies(&[state(&rigid, body_b), state(&rigid, body_a)])
            .unwrap();
        let dt = frame.next_substep().unwrap().unwrap();
        frame.advance(dt).unwrap();
        let reactions = frame.rigid_reactions().unwrap();
        assert_eq!(reactions.len(), 2);
        rigid
            .apply_impulses(&[
                reactions[0].body_impulse(body_b).unwrap(),
                reactions[1].body_impulse(body_a).unwrap(),
            ])
            .unwrap();
        assert_response(before_b, rigid.dynamics(body_b).unwrap(), reactions[0]);
        assert_response(before_a, rigid.dynamics(body_a).unwrap(), reactions[1]);
        for (sum, reaction) in total.iter_mut().zip(reactions) {
            *sum += reaction
                .linear
                .iter()
                .chain(&reaction.angular)
                .map(|value| value.abs())
                .sum::<f64>();
        }
        rigid.step(dt, 1).unwrap();
        remaining -= dt.0;
    }
    assert!(total.iter().all(|sum| *sum > 1e-3));
    assert_eq!(frame.next_substep().unwrap(), None);
    frame.finish().unwrap();
}
