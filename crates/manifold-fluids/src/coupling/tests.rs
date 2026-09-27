use super::*;
use crate::{Bounds, Config, LiquidOptions, Seconds, TimeStepOptions};
use manifold_physics::{BodyConfig, PhysicsWorld, TriangleMesh};

mod boundaries;
mod gravity;

const DT: Seconds = Seconds(1.0 / 60.0);

fn proxy() -> TriangleMesh {
    let [x, y, z] = [0.25, 0.2, 0.225];
    TriangleMesh {
        vertices: vec![
            [-x, -y, -z],
            [x, -y, -z],
            [x, y, -z],
            [-x, y, -z],
            [-x, -y, z],
            [x, -y, z],
            [x, y, z],
            [-x, y, z],
        ],
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ],
    }
}

struct Pair {
    fluid: FluidWorld,
    rigid: PhysicsWorld,
    body: BodyHandle,
    collider: MeshHandle,
}

impl Pair {
    fn new(ratio: f32, viscosity: f64, kind: BodyKind, density: f64, emit: bool) -> Self {
        let mesh = proxy();
        let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
        let angle: f32 = 0.23;
        let body = rigid
            .add_hull(
                &mesh.vertices,
                BodyConfig {
                    kind,
                    position: [1.2, 1.05, 1.2],
                    rotation: [0.0, angle.sin(), 0.0, angle.cos()],
                    mass: 0.5 * 0.4 * 0.45 * 1000.0 * ratio,
                    ..BodyConfig::default()
                },
            )
            .unwrap();
        let mut fluid = FluidWorld::new(Config {
            cells: [16; 3],
            cell_size: 0.15,
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
        let collider = fluid
            .add_mesh(&mesh, MeshRole::Collider, rigid.pose(body).unwrap())
            .unwrap();
        fluid.set_collider_friction(collider, 1.0).unwrap();
        if emit {
            fluid
                .add_fluid_box(
                    Bounds {
                        min: [0.45; 3],
                        max: [1.95, 1.65, 1.95],
                    },
                    [0.0; 3],
                )
                .unwrap();
            // Emit and settle the velocity transfer before enabling exchange.
            fluid.step(DT).unwrap();
        }
        fluid.prepare_rigid_coupling(&[collider], density).unwrap();
        Self {
            fluid,
            rigid,
            body,
            collider,
        }
    }

    fn state(&self) -> RigidBodyState {
        RigidBodyState {
            pose: self.rigid.pose(self.body).unwrap(),
            dynamics: self.rigid.dynamics(self.body).unwrap(),
        }
    }
}

#[test]
fn production_coupling_exchanges_pressure_and_viscosity_with_box3d() {
    for ratio in [0.1, 1.0, 10.0] {
        for viscosity in [0.0, 1.0] {
            let mut pair = Pair::new(ratio, viscosity, BodyKind::Dynamic, 1000.0, true);
            pair.rigid
                .set_velocity(pair.body, [0.6, 0.0, 0.0], [0.0, 0.4, 0.0])
                .unwrap();
            let initial_pose = pair.rigid.pose(pair.body).unwrap();
            let mut total_reaction = 0.0;
            let mut max_response_error: f64 = 0.0;
            for _ in 0..3 {
                let mut frame = pair.fluid.begin_frame(DT).unwrap();
                let mut elapsed = 0.0;
                while elapsed < DT.0 - 1e-12 {
                    let before = pair.rigid.dynamics(pair.body).unwrap();
                    frame
                        .set_rigid_bodies(&[RigidBodyState {
                            pose: pair.rigid.pose(pair.body).unwrap(),
                            dynamics: before,
                        }])
                        .unwrap();
                    assert!(
                        frame.rigid_reactions().is_err(),
                        "upload must invalidate old reactions"
                    );
                    let dt = frame.next_substep().unwrap().unwrap();
                    frame.advance(dt).unwrap();
                    let reaction = frame.rigid_reactions().unwrap()[0];
                    total_reaction += reaction
                        .linear
                        .iter()
                        .chain(reaction.angular.iter())
                        .map(|value| value.abs())
                        .sum::<f64>();
                    pair.rigid
                        .apply_impulses(&[reaction.body_impulse(pair.body).unwrap()])
                        .unwrap();
                    let after = pair.rigid.dynamics(pair.body).unwrap();
                    for axis in 0..3 {
                        max_response_error = max_response_error.max(
                            (f64::from(after.linear_velocity[axis] - before.linear_velocity[axis])
                                - reaction.delta_linear[axis])
                                .abs(),
                        );
                        max_response_error = max_response_error.max(
                            (f64::from(
                                after.angular_velocity[axis] - before.angular_velocity[axis],
                            ) - reaction.delta_angular[axis])
                                .abs(),
                        );
                    }
                    pair.rigid.step(dt, 1).unwrap();
                    elapsed += dt.0;
                }
                assert_eq!(frame.next_substep().unwrap(), None);
                frame.finish().unwrap();
            }
            println!(
                "ratio={ratio} viscosity={viscosity} reaction={total_reaction} response_error={max_response_error}"
            );
            assert!(total_reaction > 1e-3, "fixture must exchange momentum");
            assert!(
                max_response_error < 5e-5,
                "native body must match the coupled solve"
            );
            let pose = pair.rigid.pose(pair.body).unwrap();
            assert!((pose.position[0] - initial_pose.position[0]).abs() > 1e-5);
            assert!(
                pose.position
                    .iter()
                    .chain(pose.rotation.iter())
                    .all(|value| value.is_finite())
            );
            let mut surface = Vec::new();
            pair.fluid.surface(&mut surface).unwrap();
            assert!(!surface.is_empty());
        }
    }
}

#[test]
fn production_coupling_requires_fresh_states_and_rejects_stale_reactions() {
    let mut pair = Pair::new(1.0, 0.0, BodyKind::Dynamic, 1000.0, true);
    let state = pair.state();
    let mut frame = pair.fluid.begin_frame(DT).unwrap();
    assert!(frame.next_substep().is_err());
    assert!(frame.rigid_reactions().is_err());
    frame.set_rigid_bodies(&[state]).unwrap();
    let dt = frame.next_substep().unwrap().unwrap();
    assert!(
        frame.set_rigid_bodies(&[state]).is_err(),
        "a pending CFL offer freezes its inputs"
    );
    assert_eq!(frame.next_substep().unwrap(), Some(dt));
    frame.advance(dt).unwrap();
    assert_eq!(frame.rigid_reactions().unwrap().len(), 1);
    assert!(
        frame.next_substep().is_err(),
        "next substep needs a new body snapshot"
    );
    for value in [f32::NAN, -1.0] {
        let mut invalid = state;
        invalid.dynamics.inverse_inertia[0][0] = value;
        assert!(frame.set_rigid_bodies(&[invalid]).is_err());
        assert!(
            frame.rigid_reactions().is_err(),
            "failed upload cannot expose old reaction"
        );
    }
    let mut asymmetric = state;
    asymmetric.dynamics.inverse_inertia[0][2] += 0.001;
    assert!(frame.set_rigid_bodies(&[asymmetric]).is_err());
    let mut invalid_acceleration = state;
    invalid_acceleration.dynamics.external_linear_acceleration[2] = f32::NAN;
    assert!(frame.set_rigid_bodies(&[invalid_acceleration]).is_err());
    frame.set_rigid_bodies(&[state]).unwrap();
    let dt = frame.next_substep().unwrap().unwrap();
    frame.advance(dt).unwrap();
    assert_eq!(frame.next_substep().unwrap(), None);
    frame.finish().unwrap();
    assert!(
        pair.fluid.step(DT).is_err(),
        "coupled world needs an exchange owner"
    );
    let mut next_frame = pair.fluid.begin_frame(DT).unwrap();
    assert!(
        next_frame.rigid_reactions().is_err(),
        "a new frame cannot expose the previous frame's last reaction"
    );
    assert!(next_frame.next_substep().is_err());
}

#[test]
fn production_coupling_empty_and_disabled_bodies_have_zero_reaction() {
    for emit in [false, true] {
        let mut pair = Pair::new(1.0, 1.0, BodyKind::Dynamic, 1000.0, emit);
        let mut state = pair.state();
        if emit {
            state.dynamics.enabled = false;
        }
        let mut frame = pair.fluid.begin_frame(DT).unwrap();
        for _ in 0..2 {
            frame.set_rigid_bodies(&[state]).unwrap();
            let dt = frame.next_substep().unwrap().unwrap();
            frame.advance(dt).unwrap();
            assert_eq!(
                frame.rigid_reactions().unwrap(),
                &[RigidReaction::default()]
            );
        }
        assert_eq!(frame.next_substep().unwrap(), None);
        frame.finish().unwrap();
    }
}

#[test]
fn production_coupling_prescribed_reactions_scale_with_density() {
    let mut outputs = Vec::new();
    for density in [500.0, 1000.0] {
        let mut pair = Pair::new(1.0, 1.0, BodyKind::Animated, density, true);
        pair.rigid
            .set_velocity(pair.body, [0.6, 0.0, 0.0], [0.0, 0.4, 0.0])
            .unwrap();
        let state = pair.state();
        let mut total = [0.0; 6];
        let mut frame = pair.fluid.begin_frame(DT).unwrap();
        for _ in 0..2 {
            frame.set_rigid_bodies(&[state]).unwrap();
            let dt = frame.next_substep().unwrap().unwrap();
            frame.advance(dt).unwrap();
            let reaction = frame.rigid_reactions().unwrap()[0];
            assert_eq!(reaction.delta_linear, [0.0; 3]);
            assert_eq!(reaction.delta_angular, [0.0; 3]);
            for (sum, value) in total
                .iter_mut()
                .zip(reaction.linear.into_iter().chain(reaction.angular))
            {
                *sum += value;
            }
        }
        frame.finish().unwrap();
        outputs.push(total);
    }
    assert!(outputs[0].iter().any(|value| value.abs() > 1e-3));
    for (low, high) in outputs[0].into_iter().zip(outputs[1]) {
        assert!(
            (high - 2.0 * low).abs() <= 1e-4 * high.abs().max(1.0),
            "density scaling: {low} vs {high}"
        );
    }
}

#[test]
fn production_coupling_guards_bound_topology_and_abandonment() {
    let mut pair = Pair::new(1.0, 0.0, BodyKind::Dynamic, 1000.0, true);
    let pose = pair.rigid.pose(pair.body).unwrap();
    assert!(pair.fluid.remove_mesh(pair.collider).is_err());
    assert!(pair.fluid.set_mesh_enabled(pair.collider, false).is_err());
    let moved = BodyPose {
        position: [1.3, 1.05, 1.2],
        ..pose
    };
    assert!(
        pair.fluid
            .set_mesh_motion(pair.collider, moved, moved, moved)
            .is_err()
    );
    let state = pair.state();
    let mut frame = pair.fluid.begin_frame(DT).unwrap();
    frame.set_rigid_bodies(&[state]).unwrap();
    let dt = frame.next_substep().unwrap().unwrap();
    frame.advance(dt).unwrap();
    drop(frame);
    assert!(pair.fluid.begin_frame(DT).is_err());
    assert!(pair.fluid.surface(&mut Vec::new()).is_err());
}
