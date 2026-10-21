use manifold_core::{PlayedTime, Seconds};
use manifold_physics::input::EventStamp;

use manifold_core::scene_impulse::RigidImpulseTargets;
use crate::physics::{copies_scene, targets, ResolvedRigidImpulse, RigidBody, RigidSimulation, SimStep, IMPULSE_CAPACITY};
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

fn one_body(position: [f32; 3]) -> Vec<Option<RigidBody>> {
    let mut bodies = vec![None; 64];
    bodies[0] = Some(body(position));
    bodies
}

fn initialize(simulation: &mut RigidSimulation, bodies: &[Option<RigidBody>]) {
    simulation
        .advance(bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
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
        .impulse_stamp(PlayedTime::ZERO, 0)
        .expect_err("stamp before initialization")
        .contains("epoch"));

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(5.0), 1.0, 0.0)
        .unwrap();
    let epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(
        simulation.impulse_stamp(PlayedTime::from_played(5.0), 1).unwrap(),
        EventStamp {
            epoch,
            time: Seconds::ZERO,
            sequence: 1,
        }
    );

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(6.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        simulation.impulse_stamp(PlayedTime::from_played(6.0), 2).unwrap().time,
        Seconds(1.0)
    );
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(6.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        simulation.impulse_stamp(PlayedTime::from_played(6.0), 3).unwrap().time,
        Seconds(1.0)
    );
    assert!(simulation.impulse_stamp(PlayedTime::from_played(6.0 + 1e-9), 4).is_err());
}

#[test]
fn rigid_impulse_stamp_stays_invalid_for_authored_only_rebuild_and_pending() {
    let mut bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    assert!(simulation.impulse_stamp(PlayedTime::ZERO, 0).is_ok());

    simulation.hold_pending(PlayedTime::from_played(1.0));
    assert!(simulation.impulse_stamp(PlayedTime::from_played(1.0), 1).is_err());

    let body = bodies[0].as_mut().unwrap();
    body.shape = (body.shape + 1) % 5;
    simulation.set_step(SimStep::default().authored_sample());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0), 1.0, 0.0)
        .unwrap();
    simulation.set_step(SimStep::default());
    assert!(simulation.impulse_stamp(PlayedTime::from_played(2.0), 2).is_err());

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0), 1.0, 0.0)
        .unwrap();
    let stamp = simulation.impulse_stamp(PlayedTime::from_played(2.0), 3).unwrap();
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
                    targets(1, false),
                ),
            )
            .unwrap();
    }

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
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
                targets(1, false),
            ),
        )
        .unwrap();
    assert_eq!(planned.tick, 1);

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 0.0)
        .unwrap();
    let applied = receipts(&mut simulation);
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].applied.tick, 1);

    let mut late = RigidSimulation::default();
    let bodies = one_body([0.0, 4.0, 0.0]);
    initialize(&mut late, &bodies);
    let epoch = late.impulse_epoch().unwrap();
    late.advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    late.enqueue_impulse(
        stamp(epoch, 0.25 * DT, 0),
        payload(
            [1.0, 0.0, 0.0],
            targets(1, false),
        ),
    )
    .unwrap();
    late.advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 0.0)
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
    let mut targets = targets(1, false);
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 0),
            ResolvedRigidImpulse {
                field: manifold_physics::FieldValue::uniform([2.0, 0.0, 0.0]).unwrap(),
                targets,
            },
        )
        .unwrap();
    targets = RigidImpulseTargets::default();
    assert!(targets.is_empty());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    assert!(simulation.poses[0].pos[0] > 0.02);
    let applied = receipts(&mut simulation);
    assert_eq!(applied[0].value.targets, RigidImpulseTargets::body(0).unwrap());
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
                targets(1, false),
            ),
        )
        .unwrap();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 1.0)
        .unwrap();
    let second_epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(second_epoch, first_epoch + 1);
    assert!(simulation
        .enqueue_impulse(
            stamp(first_epoch, 0.0, 1),
            payload(
                [1.0, 0.0, 0.0],
                targets(1, false)
            ),
        )
        .is_err());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 1.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 1.0)
        .unwrap();
    let third_epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(third_epoch, second_epoch + 1);
    assert!(simulation
        .enqueue_impulse(
            stamp(second_epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                targets(1, false)
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
        .advance_scene(&copies_scene(&bodies, Some(prototype.clone()), 1.0, 1.25, 16.0, 0.0, [0.0; 3], None, &[]), PlayedTime::ZERO, 1.0, 0.0)
        .unwrap();
    let copy_origin = simulation.group_poses(0)[0].pos[0];
    let epoch = simulation.impulse_epoch().unwrap();
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                targets(1, true),
            ),
        )
        .unwrap();
    simulation
        .advance_scene(&copies_scene(&bodies, Some(prototype), 1.0, 1.25, 16.0, 0.0, [0.0; 3], None, &[]), PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    assert!((simulation.poses[0].pos[0] + 2.0 - DT as f32).abs() < 1e-5);
    assert!((simulation.group_poses(0)[0].pos[0] - copy_origin - DT as f32).abs() < 1e-5);
    assert_eq!(
        simulation.poses[1].pos[0], 10.0,
        "untargeted peer must stay still"
    );
    let world = simulation.world.as_ref().unwrap();
    for handle in [
        simulation.handles[0].unwrap(),
        simulation.native_copy_handles(0)[0].unwrap(),
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
                targets(1, false),
            ),
        )
        .unwrap();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::ZERO, 0.0, 0.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    assert!(receipts(&mut simulation).is_empty());
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 0.0)
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
                targets(3, false),
            ),
        )
        .unwrap();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    assert!(
        simulation
            .configure_fast_bodies(&bodies, &[None], [0.0; 3], None, None, DT)
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
fn rigid_impulses_full_history_refuses_one_hit_and_keeps_stepping() {
    let bodies = one_body([0.0, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    let targets = targets(1, false);
    for sequence in 0..256 {
        simulation.enqueue_impulse(stamp(epoch, 0.0, sequence), payload([0.0; 3], targets)).unwrap();
    }
    let error = simulation.enqueue_impulse(stamp(epoch, 0.0, 256), payload([0.0; 3], targets)).unwrap_err();
    assert!(error.contains("refused"), "{error}");
    // One refusal latches nothing: the world keeps stepping and every
    // admitted hit lands once.
    simulation.advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0).unwrap();
    assert_eq!(simulation.impulse_readiness(), Ok(true));
    // Undrained receipts still hold their slots.
    assert!(simulation.enqueue_impulse(stamp(epoch, DT, 257), payload([0.0; 3], targets)).is_err());
    assert_eq!(receipts(&mut simulation).len(), 256);
    // Drained, the history frees and the next hit is admitted and lands once.
    simulation.enqueue_impulse(stamp(epoch, DT, 258), payload([0.0; 3], targets)).unwrap();
    simulation.advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 0.0).unwrap();
    let landed: Vec<_> = receipts(&mut simulation).iter().map(|event| event.source.sequence).collect();
    assert_eq!(landed, [258]);
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
                targets(1 << 1, false)
            ),
        )
        .is_err());
    simulation
        .enqueue_impulse(
            stamp(epoch, 0.0, 7),
            payload(
                [1.0, 0.0, 0.0],
                targets(1, false),
            ),
        )
        .unwrap();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
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
                targets: targets(1, false),
            },
        )
        .unwrap();
    assert!(simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .is_err());
    assert_eq!(receipts(&mut simulation).len(), 1);
    assert!(simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 0.0)
        .is_err());

    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 1.0)
        .unwrap();
    let second_epoch = simulation.impulse_epoch().unwrap();
    assert_eq!(second_epoch, first_epoch + 1);
    simulation
        .enqueue_impulse(
            stamp(second_epoch, 0.0, 0),
            payload(
                [1.0, 0.0, 0.0],
                targets(1, false),
            ),
        )
        .unwrap();
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 1.0, 1.0)
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
            .impulse_stamp(PlayedTime::from_played(transport), sequence)
            .unwrap();
        let targets = targets(1, false);
        simulation
            .enqueue_impulse(stamp, payload([4.0, 0.0, 0.0], targets))
            .unwrap();
    };
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    // Paused: the transport repeats.
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(DT), 1.0, 0.0)
        .unwrap();
    strike(&mut simulation, DT, 0);
    // Simulation Speed 0 while the transport keeps running.
    simulation
        .advance(&bodies, [0.0; 3], PlayedTime::from_played(2.0 * DT), 0.0, 0.0)
        .unwrap();
    strike(&mut simulation, 2.0 * DT, 1);
    assert_eq!(
        simulation.impulse_queue.as_ref().unwrap().len(),
        0,
        "held impulses pend"
    );
    let discarded: Vec<_> = simulation.drain_discarded_impulses().map(|stamp| stamp.sequence).collect();
    assert_eq!(discarded, [0, 1], "each held hit leaves one discard receipt");
    // Receipts are pre-sized and bounded: a held hit never allocates, and
    // one past the bound is counted without a stamp.
    let capacity = simulation.impulse_discarded.capacity();
    assert!(capacity >= IMPULSE_CAPACITY);
    for sequence in 2..IMPULSE_CAPACITY as u64 + 7 {
        strike(&mut simulation, 2.0 * DT, sequence);
    }
    assert_eq!(simulation.impulse_discarded.capacity(), capacity, "a held hit never allocates");
    assert_eq!(simulation.drain_discarded_impulses().count(), IMPULSE_CAPACITY);
    assert_eq!(simulation.take_discarded_beyond(), 5, "a discard past the bound is still counted");
    assert_eq!(simulation.take_discarded_beyond(), 0, "counted once");

    for frame in 3..8 {
        simulation
            .advance(&bodies, [0.0; 3], PlayedTime::from_played(frame as f64 * DT), 1.0, 0.0)
            .unwrap();
    }
    assert!(
        receipts(&mut simulation).is_empty(),
        "resume replayed a held impulse"
    );
    assert_eq!(simulation.poses[0].pos[0], 0.0, "{:?}", simulation.poses[0]);
}

/// An Explosion fire is one radial delta velocity, Impulse Size times the
/// Edge falloff at the body: it lands on the tick it was stamped for and
/// never again, however many ticks run after it.
#[test]
fn explosion_impulse_lands_once_per_fire() {
    let bodies = one_body([1.5, 4.0, 0.0]);
    let mut simulation = RigidSimulation::default();
    initialize(&mut simulation, &bodies);
    let epoch = simulation.impulse_epoch().unwrap();
    let explosion = manifold_physics::FieldValue::radial([0.0, 4.0, 0.0], 3.0, 1.0)
        .and_then(|field| field.scaled(8.0))
        .unwrap();
    let targets = targets(1, false);
    simulation
        .enqueue_impulse(stamp(epoch, 0.0, 0), ResolvedRigidImpulse { field: explosion, targets })
        .unwrap();
    let speed = |simulation: &RigidSimulation| {
        simulation.world.as_ref().unwrap().linear_velocity(simulation.handles[0].unwrap()).unwrap()
    };
    let mut landed = Vec::new();
    for frame in 1..=6 {
        simulation
            .advance(&bodies, [0.0; 3], PlayedTime::from_played(frame as f64 * DT), 1.0, 0.0)
            .unwrap();
        landed.extend(receipts(&mut simulation));
        // 8 × (1 − 1.5 / 3): outward along +x, no matter how many ticks follow.
        let velocity = speed(&simulation);
        assert!((velocity[0] - 4.0).abs() < 1e-3, "frame {frame}: velocity={velocity:?}");
        assert!(velocity[1].abs() < 1e-4 && velocity[2].abs() < 1e-4, "frame {frame}: velocity={velocity:?}");
    }
    assert_eq!(landed.len(), 1, "one fire, one landing");
    assert_eq!(landed[0].applied.tick, 0);
}
