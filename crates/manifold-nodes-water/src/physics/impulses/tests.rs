use manifold_core::Seconds;
use manifold_physics::input::EventStamp;

use manifold_node_engine::scene::impulse::RigidImpulseTargets;
use crate::physics::{PhysicsAuthoredSampleScope, ResolvedRigidImpulse, RigidBody, RigidSimulation, MAX_BODIES};
use manifold_node_engine::scene::transform::Transform;

const DT: f64 = 1.0 / 60.0;

fn body(position: [f32; 3]) -> RigidBody {
    RigidBody {
        transform: Transform {
            pos: position,
            ..Transform::default()
        },
        ..RigidBody::default()
    }
}

fn one_body(position: [f32; 3]) -> [Option<RigidBody>; MAX_BODIES] {
    let mut bodies = std::array::from_fn(|_| None);
    bodies[0] = Some(body(position));
    bodies
}

fn initialize(simulation: &mut RigidSimulation, bodies: &[Option<RigidBody>; MAX_BODIES]) {
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
        .unwrap();
}

fn payload(vector: [f32; 3], targets: RigidImpulseTargets) -> ResolvedRigidImpulse {
    ResolvedRigidImpulse {
        field: manifold_physics::FieldValue::uniform(vector).unwrap(),
        targets,
    }
}

fn stamp(epoch: u64, time: f64, sequence: u64) -> EventStamp {
    EventStamp {
        epoch,
        time: Seconds(time),
        sequence,
    }
}

fn receipts(
    simulation: &mut RigidSimulation,
) -> Vec<manifold_physics::input::AppliedEvent<ResolvedRigidImpulse>> {
    simulation.drain_applied_impulses().collect()
}

#[test]
fn rigid_impulse_stamp_requires_an_exact_accepted_observation() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    assert!(simulation
        .impulse_stamp(Seconds::ZERO, 0)
        .expect_err("stamp before initialization")
        .contains("epoch"));

    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(5.0), 1.0, 0.0)
        .unwrap();
    let epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(
        simulation.impulse_stamp(Seconds(5.0), 1).unwrap(),
        EventStamp {
            epoch,
            time: Seconds::ZERO,
            sequence: 1,
        }
    );

    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(6.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        simulation.impulse_stamp(Seconds(6.0), 2).unwrap().time,
        Seconds(1.0)
    );
    simulation
        .advance(bodies, [0.0; 3], Seconds(6.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        simulation.impulse_stamp(Seconds(6.0), 3).unwrap().time,
        Seconds(1.0)
    );
    assert!(simulation.impulse_stamp(Seconds(6.0 + 1e-9), 4).is_err());
}

#[test]
fn rigid_impulse_stamp_stays_invalid_for_authored_only_rebuild_and_pending() {
    let mut bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 1.0, 0.0)
        .unwrap();
    assert!(simulation.impulse_stamp(Seconds::ZERO, 0).is_ok());

    simulation.hold_pending(Seconds(1.0));
    assert!(simulation.impulse_stamp(Seconds(1.0), 1).is_err());

    let body = bodies[0].as_mut().unwrap();
    body.shape = (body.shape + 1) % 5;
    {
        let _scope = PhysicsAuthoredSampleScope::new();
        simulation
            .advance(bodies.clone(), [0.0; 3], Seconds(2.0), 1.0, 0.0)
            .unwrap();
    }
    assert!(simulation.impulse_stamp(Seconds(2.0), 2).is_err());

    simulation
        .advance(bodies, [0.0; 3], Seconds(2.0), 1.0, 0.0)
        .unwrap();
    let stamp = simulation.impulse_stamp(Seconds(2.0), 3).unwrap();
    assert_eq!(stamp.time, Seconds::ZERO);
}

#[test]
fn rigid_impulses_multiple_hits_sum_into_native_position() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    for sequence in 0..2 {
        simulation
            .enqueue_impulse(
                stamp(epoch, 0.0, sequence),
                payload(
                    [1.0, 0.0, 0.0],
                    RigidImpulseTargets {
                        bodies: 1,
                        copies: false,
                    },
                ),
            )
            .unwrap();
    }

    simulation
        .advance(bodies, [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert!(
        simulation.poses[0].pos[0] > 0.01,
        "native position: {:?}",
        simulation.poses[0]
    );
    let velocity = simulation
        .world
        .as_ref()
        .unwrap()
        .linear_velocity(simulation.handles[0].unwrap())
        .unwrap();
    assert!((velocity[0] - 2.0).abs() < 1e-5, "velocity={velocity:?}");
    assert!((simulation.poses[0].pos[0] - (2.0 * DT) as f32).abs() < 1e-5);
    let applied = receipts(&mut simulation);
    assert_eq!(applied.len(), 2);
    assert!(applied.iter().all(|event| event.applied.tick == 0));
}

#[test]
fn rigid_impulses_half_open_boundary_and_late_assignment() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    let planned = simulation
        .enqueue_impulse(
            stamp(epoch, DT, 0),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            ),
        )
        .unwrap();
    assert_eq!(planned.tick, 1);

    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());
    simulation
        .advance(bodies, [0.0; 3], Seconds(2.0 * DT), 1.0, 0.0)
        .unwrap();
    let applied = receipts(&mut simulation);
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].applied.tick, 1);

    let mut late = RigidSimulation::default();
    let bodies = one_body([0.0, 4.0, 0.0]);
    initialize(&mut late, &bodies);
    let epoch = late.impulse_epoch().unwrap();
    late.advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    late.enqueue_impulse(
        stamp(epoch, 0.25 * DT, 0),
        payload(
            [1.0, 0.0, 0.0],
            RigidImpulseTargets {
                bodies: 1,
                copies: false,
            },
        ),
    )
    .unwrap();
    late.advance(bodies, [0.0; 3], Seconds(2.0 * DT), 1.0, 0.0)
        .unwrap();
    let applied = receipts(&mut late);
    assert_eq!(applied.len(), 1);
    assert!((applied[0].lateness.0 - 0.75 * DT).abs() < 1.0e-12);
}

#[test]
fn rigid_impulses_capture_field_and_targets_by_value() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    let mut targets = RigidImpulseTargets {
        bodies: 1,
        copies: false,
    };
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 0),
            ResolvedRigidImpulse {
                field: manifold_physics::FieldValue::uniform([2.0, 0.0, 0.0]).unwrap(),
                targets,
            },
        )
        .unwrap();
    targets.bodies = 0;
    assert!(targets.is_empty());
    simulation
        .advance(bodies, [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[0] > 0.02);
    let applied = receipts(&mut simulation);
    assert_eq!(applied[0].value.targets.bodies, 1);
}

#[test]
fn rigid_impulses_reset_and_seek_cancel_old_epoch_inputs() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let first_epoch = simulation.impulse_epoch().unwrap();
    simulation
        .enqueue_impulse(
            stamp(first_epoch, 0.0, 0),
            payload(
                [3.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            ),
        )
        .unwrap();
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 1.0)
        .unwrap();
    let second_epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(second_epoch, first_epoch + 1);
    assert!(simulation
        .enqueue_impulse(
            stamp(first_epoch, 0.0, 1),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false
                }
            ),
        )
        .is_err());
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(2.0 * DT), 1.0, 1.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());

    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 1.0)
        .unwrap();
    let third_epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(third_epoch, second_epoch + 1);
    assert!(simulation
        .enqueue_impulse(
            stamp(second_epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false
                }
            ),
        )
        .is_err());
}

#[test]
fn rigid_impulses_target_body_and_copies_in_one_batch() {
    let mut bodies = one_body([-2.0, 4.0, 0.0]);
    bodies[1] = Some(body([10.0, 4.0, 0.0]));
    let prototype = body([2.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance_with_copies(
            bodies.clone(),
            Some(prototype.clone()),
            1.0,
            1.25,
            16.0,
            [0.0; 3],
            Seconds::ZERO,
            1.0,
            0.0,
        )
        .unwrap();
    let copy_origin = simulation.copy_poses[0].pos[0];
    let epoch = simulation.impulse_epoch().unwrap();
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: true,
                },
            ),
        )
        .unwrap();
    simulation
        .advance_with_copies(
            bodies,
            Some(prototype),
            1.0,
            1.25,
            16.0,
            [0.0; 3],
            Seconds(DT),
            1.0,
            0.0,
        )
        .unwrap();
    assert!((simulation.poses[0].pos[0] + 2.0 - DT as f32).abs() < 1e-5);
    assert!((simulation.copy_poses[0].pos[0] - copy_origin - DT as f32).abs() < 1e-5);
    assert_eq!(
        simulation.poses[1].pos[0], 10.0,
        "untargeted peer must stay still"
    );
    let world = simulation.world.as_ref().unwrap();
    for handle in [
        simulation.handles[0].unwrap(),
        simulation.copy_handles[0].unwrap(),
    ] {
        assert!((world.linear_velocity(handle).unwrap()[0] - 1.0).abs() < 1e-5);
    }
    assert_eq!(receipts(&mut simulation).len(), 1);
}

#[test]
fn rigid_impulses_pause_retains_pending_input() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            ),
        )
        .unwrap();
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds::ZERO, 0.0, 0.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());
    simulation
        .advance(bodies, [0.0; 3], Seconds(2.0 * DT), 1.0, 0.0)
        .unwrap();
    assert_eq!(receipts(&mut simulation).len(), 1);
}

#[test]
fn rigid_impulses_high_velocity_applies_once_across_microsteps() {
    let mut bodies = one_body([0.0, 4.0, 0.0]);
    bodies[1] = Some(body([5.0, 4.0, 0.0]));
    for body in bodies.iter_mut().flatten() {
        body.transform.scale = [0.02; 3];
    }
    bodies[1].as_mut().unwrap().density *= 10.0;
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 0),
            payload(
                [30.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 3,
                    copies: false,
                },
            ),
        )
        .unwrap();
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert!(
        simulation
            .configure_fast_bodies(&bodies, None, [0.0; 3], None, None, DT)
            .unwrap()
            > 1,
        "this fixture must select native microsteps"
    );
    for (index, origin) in [(0, 0.0), (1, 5.0)] {
        let velocity = simulation
            .world
            .as_ref()
            .unwrap()
            .linear_velocity(simulation.handles[index].unwrap())
            .unwrap();
        assert!((velocity[0] - 30.0).abs() < 1e-4, "velocity={velocity:?}");
        assert!((simulation.poses[index].pos[0] - origin - 0.5).abs() < 1e-4);
    }
    assert_eq!(receipts(&mut simulation).len(), 1);
}

#[test]
fn rigid_impulses_queue_and_receipt_budget_overflow_is_sticky() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    for sequence in 0..256 {
        simulation
            .enqueue_impulse(
                stamp(epoch, 0.0, sequence),
                payload(
                    [0.0; 3],
                    RigidImpulseTargets {
                        bodies: 1,
                        copies: false,
                    },
                ),
            )
            .unwrap();
    }
    assert!(simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 256),
            payload(
                [0.0; 3],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false
                }
            ),
        )
        .is_err());
    assert!(simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .is_err());
    assert_eq!(receipts(&mut simulation).len(), 0);

    let mut recovered = RigidSimulation::default();
    initialize(&mut recovered, &bodies);
    let epoch = recovered.impulse_epoch().unwrap();
    for sequence in 0..256 {
        recovered
            .enqueue_impulse(
                stamp(epoch, 0.0, sequence),
                payload(
                    [0.0; 3],
                    RigidImpulseTargets {
                        bodies: 1,
                        copies: false,
                    },
                ),
            )
            .unwrap();
    }
    recovered
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert_eq!(recovered.impulse_receipts.len(), 256);
    assert!(recovered
        .enqueue_impulse(
            stamp(epoch, DT, 256),
            payload(
                [0.0; 3],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false
                }
            ),
        )
        .is_err());
    assert_eq!(receipts(&mut recovered).len(), 256);
    assert!(recovered
        .advance(bodies.clone(), [0.0; 3], Seconds(2.0 * DT), 1.0, 0.0)
        .is_err());
    recovered
        .advance(bodies, [0.0; 3], Seconds(2.0 * DT), 1.0, 1.0)
        .unwrap();
    assert_eq!(recovered.impulse_queue.as_ref().unwrap().len(), 0);
    assert!(!recovered.impulse_overflow_latched);
}

#[test]
fn rigid_impulses_invalid_target_preserves_producer_sequence() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    assert!(simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 7),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1 << 1,
                    copies: false
                }
            ),
        )
        .is_err());
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 7),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            ),
        )
        .unwrap();
    simulation
        .advance(bodies, [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    assert_eq!(receipts(&mut simulation).len(), 1);
}

#[test]
fn rigid_impulses_native_error_latches_receipt_and_reset_recovers() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let first_epoch = simulation.impulse_epoch().unwrap();
    let overflowing_field = manifold_physics::FieldValue::uniform([f32::MAX; 3])
        .unwrap()
        .scaled(2.0)
        .unwrap();
    simulation
        .enqueue_impulse(
            stamp(first_epoch, 0.0, 0),
            ResolvedRigidImpulse {
                field: overflowing_field,
                targets: RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            },
        )
        .unwrap();
    assert!(simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .is_err());
    assert_eq!(receipts(&mut simulation).len(), 1);
    assert!(simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(2.0 * DT), 1.0, 0.0)
        .is_err());

    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 1.0)
        .unwrap();
    let second_epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(second_epoch, first_epoch + 1);
    simulation
        .enqueue_impulse(
            stamp(second_epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            ),
        )
        .unwrap();
    simulation
        .advance(bodies, [0.0; 3], Seconds(2.0 * DT), 1.0, 1.0)
        .unwrap();
    assert_eq!(receipts(&mut simulation).len(), 1);
}

/// WATER_SIMULATION_DESIGN.md "Transport pause / water speed zero": held
/// rigid bodies discard incoming impulses, so resume never fires a pile.
#[test]
fn rigid_impulses_while_held_are_discarded_not_replayed_on_resume() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let strike = |simulation: &mut RigidSimulation, transport: f64, sequence: u64| {
        let stamp = simulation
            .impulse_stamp(Seconds(transport), sequence)
            .unwrap();
        let targets = RigidImpulseTargets {
            bodies: 1,
            copies: false,
        };
        simulation
            .enqueue_impulse(stamp, payload([4.0, 0.0, 0.0], targets))
            .unwrap();
    };
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    // Paused: the transport repeats.
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(DT), 1.0, 0.0)
        .unwrap();
    strike(&mut simulation, DT, 0);
    // Simulation Speed 0 while the transport keeps running.
    simulation
        .advance(bodies.clone(), [0.0; 3], Seconds(2.0 * DT), 0.0, 0.0)
        .unwrap();
    strike(&mut simulation, 2.0 * DT, 1);
    assert_eq!(
        simulation.impulse_queue.as_ref().unwrap().len(),
        0,
        "held impulses pend"
    );

    for frame in 3..8 {
        simulation
            .advance(
                bodies.clone(),
                [0.0; 3],
                Seconds(frame as f64 * DT),
                1.0,
                0.0,
            )
            .unwrap();
    }
    assert!(
        receipts(&mut simulation).is_empty(),
        "resume replayed a held impulse"
    );
    assert_eq!(simulation.poses[0].pos[0], 0.0, "{:?}", simulation.poses[0]);
}
