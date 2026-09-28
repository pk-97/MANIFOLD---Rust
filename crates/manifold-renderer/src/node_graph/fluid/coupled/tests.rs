use std::sync::Arc;
use std::sync::mpsc;

use manifold_fluids::{LiquidOptions, TimeStepOptions};
use manifold_physics::input::EventStamp;
use manifold_physics::{FieldValue, Seconds, TickStamp};

use super::{CoupledRigidInputs, RigidImpulseTargets};
use crate::node_graph::fluid::native::NativeSimulation;
use crate::node_graph::fluid::{FluidControls, FluidRuntime, FluidSettings, TICK, Worker};
use crate::node_graph::physics::{ColliderGeometry, RigidBody, RigidSceneInputs};
use crate::node_graph::physics_events::{ImpulseTarget, ResolvedNodeImpulse};
use crate::node_graph::transform::Transform;

mod vortex;

const ORIGIN: [f32; 3] = [4.0, -3.0, 2.0];

fn fixture() -> (FluidSettings, FluidControls, RigidSceneInputs) {
    let position = std::array::from_fn(|axis| ORIGIN[axis] + [1.2, 1.05, 1.2][axis]);
    let settings = FluidSettings {
        resolution: 16,
        domain: Some(Transform {
            pos: ORIGIN.map(|value| value + 1.2),
            scale: [2.4; 3],
            ..Transform::default()
        }),
        fill_height: 0.0,
        initial_volume: Some(Transform {
            pos: position,
            scale: [1.5, 1.2, 1.5],
            ..Transform::default()
        }),
        liquid: LiquidOptions {
            viscosity: 0.1,
            surface_tension: 0.0,
        },
        time_steps: TimeStepOptions {
            min_substeps: 2,
            max_substeps: 32,
            cfl: 1,
            adaptive_obstacles: false,
        },
        ..FluidSettings::default()
    };
    let controls = FluidControls {
        gravity: [0.0; 3],
        emission: false,
        obstacle_enabled: false,
        ..FluidControls::default()
    };
    let mut scene = RigidSceneInputs::default();
    let hull = [-0.25, 0.25]
        .into_iter()
        .flat_map(|x| {
            [-0.2, 0.2]
                .into_iter()
                .flat_map(move |y| [-0.225, 0.225].into_iter().map(move |z| [x, y, z]))
        })
        .collect();
    scene.bodies[0] = Some(RigidBody {
        transform: Transform {
            pos: position,
            ..Transform::default()
        },
        mass: 90.0,
        bounce: 0.0,
        collider: Some(Arc::new(ColliderGeometry { hulls: vec![hull] })),
        ..RigidBody::default()
    });
    scene.acceleration_field = Some(FieldValue::uniform([2.0, 0.0, 0.0]).unwrap());
    (settings, controls, scene)
}

fn observe(
    runtime: &mut FluidRuntime,
    fixture: &(FluidSettings, FluidControls, RigidSceneInputs),
    time: f64,
    reset: f32,
    colliders: bool,
) {
    runtime
        .observe_coupled_scene_with_field(
            fixture.0,
            fixture.1,
            &[],
            None,
            Some(CoupledRigidInputs {
                scene: &fixture.2,
                colliders: RigidImpulseTargets {
                    bodies: u64::from(colliders),
                    copies: false,
                },
                density: 1000.0,
            }),
            Seconds(time),
            1.0,
            reset,
        )
        .unwrap();
}

fn trace(stalled: bool, colliders: bool) -> FluidRuntime {
    let fixture = fixture();
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, colliders);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.coupled_rigid_frame().unwrap().stamp.tick, 0);
    for sample in 1..=16 {
        observe(
            &mut runtime,
            &fixture,
            sample as f64 * TICK / 4.0,
            0.0,
            colliders,
        );
        if !stalled && sample % 4 == 0 {
            runtime.advance(true).unwrap();
            assert_eq!(
                runtime.coupled_rigid_frame().unwrap().stamp,
                TickStamp {
                    epoch: runtime.epoch,
                    tick: runtime.completed_tick
                }
            );
        }
    }
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 4);
    assert!(runtime.stats.particles > 0);
    assert!(!runtime.vertices.is_empty());
    runtime
}

#[test]
fn fluid_coupled_worker_exchanges_momentum_and_matches_stalled_publication() {
    let regular = trace(false, true);
    let stalled = trace(true, true);
    let uncoupled = trace(true, false);
    let a = regular.coupled_rigid_frame().unwrap();
    let b = stalled.coupled_rigid_frame().unwrap();
    for (&x, &y) in a.poses[0].pos.iter().zip(&b.poses[0].pos) {
        assert!((x - y).abs() < 1e-5, "regular={x}, stalled={y}");
    }
    assert_eq!(regular.vertices.len(), stalled.vertices.len());
    for (left, right) in regular.vertices.iter().zip(&stalled.vertices) {
        for (&x, &y) in left.position.iter().zip(&right.position) {
            assert!((x - y).abs() < 1e-5);
        }
    }
    let initial = fixture().2.bodies[0].as_ref().unwrap().transform.pos[0];
    let dry_x = uncoupled.coupled_rigid_frame().unwrap().poses[0].pos[0];
    let wet_x = a.poses[0].pos[0];
    assert!(
        wet_x > initial,
        "the shared force must move the coupled body"
    );
    assert!(
        dry_x - wet_x > 1e-5,
        "liquid must react on the body: wet={wet_x}, dry={dry_x}"
    );
}

fn mock_worker(
    runtime: &mut FluidRuntime,
) -> (
    mpsc::Receiver<super::super::Request>,
    mpsc::SyncSender<super::super::Reply>,
) {
    let (requests, receiver) = mpsc::sync_channel(1);
    let (sender, replies) = mpsc::sync_channel(1);
    runtime.worker = Some(Worker {
        requests,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    (receiver, sender)
}

#[test]
fn fluid_coupled_worker_cancellation_recycles_both_sides_before_new_epoch() {
    let fixture = fixture();
    let mut runtime = FluidRuntime::default();
    let (requests, replies) = mock_worker(&mut runtime);
    let mut native = NativeSimulation::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, true);
    runtime.advance(false).unwrap();
    let old = requests.recv().unwrap();
    let old_epoch = old.epoch;
    observe(&mut runtime, &fixture, TICK, 1.0, true);
    assert_ne!(runtime.epoch, old_epoch);
    let cancelled = native.process(old, &runtime.cancel_epoch);
    assert_eq!(cancelled.tick, 0);
    replies.send(cancelled).unwrap();
    runtime.advance(false).unwrap();
    assert!(runtime.coupled_rigid_frame().is_none());
    let new = requests.recv().unwrap();
    assert_eq!(new.epoch, runtime.epoch);
    replies
        .send(native.process(new, &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(
        runtime.coupled_rigid_frame().unwrap().stamp,
        TickStamp {
            epoch: runtime.epoch,
            tick: 0
        }
    );
    observe(&mut runtime, &fixture, TICK * 2.0, 1.0, true);
    runtime.advance(false).unwrap();
    let step = requests.recv().unwrap();
    replies
        .send(native.process(step, &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(
        runtime.coupled_rigid_frame().unwrap().stamp,
        TickStamp {
            epoch: runtime.epoch,
            tick: 1
        }
    );
}

#[test]
fn fluid_coupled_worker_rejects_unmatched_pair_without_partial_publication() {
    for missing in [false, true] {
        let fixture = fixture();
        let mut runtime = FluidRuntime::default();
        let (requests, replies) = mock_worker(&mut runtime);
        let mut native = NativeSimulation::default();
        observe(&mut runtime, &fixture, 0.0, 0.0, true);
        runtime.advance(false).unwrap();
        replies
            .send(native.process(requests.recv().unwrap(), &runtime.cancel_epoch))
            .unwrap();
        runtime.advance(false).unwrap();
        let poses = runtime.coupled_rigid_frame().unwrap().poses;
        let version = runtime.version;
        observe(&mut runtime, &fixture, TICK, 0.0, true);
        runtime.advance(false).unwrap();
        let mut reply = native.process(requests.recv().unwrap(), &runtime.cancel_epoch);
        assert!(reply.error.is_none(), "{:?}", reply.error);
        if missing {
            reply.coupled = None;
        } else {
            reply.coupled.as_mut().unwrap().output.stamp.tick += 1;
        }
        replies.send(reply).unwrap();
        assert!(runtime.advance(false).unwrap_err().contains("unmatched"));
        assert_eq!(runtime.completed_tick, 0);
        assert_eq!(runtime.version, version);
        assert_eq!(runtime.coupled_rigid_frame().unwrap().poses, poses);
        assert!(runtime.vertices.is_empty());
        assert!(runtime.coupled.as_ref().unwrap().spare_history.is_some());
        assert!(runtime.spare_history.is_some());
        observe(&mut runtime, &fixture, TICK, 1.0, true);
        runtime.advance(false).unwrap();
        replies
            .send(native.process(requests.recv().unwrap(), &runtime.cancel_epoch))
            .unwrap();
        runtime.advance(false).unwrap();
        assert_eq!(
            runtime.coupled_rigid_frame().unwrap().stamp,
            TickStamp {
                epoch: runtime.epoch,
                tick: 0
            }
        );
    }
}

#[test]
fn fluid_coupled_worker_keeps_historical_body_and_copy_poses_at_paused_edit() {
    let mut fixture = fixture();
    fixture.2.acceleration_field = None;
    fixture.2.bodies[0].as_mut().unwrap().kind = 2;
    fixture.2.prototype = fixture.2.bodies[0].clone();
    fixture.2.copy_count = 2.0;
    fixture.2.copy_spacing = 0.6;
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, false);
    runtime.advance(true).unwrap();
    let initial = runtime.coupled_rigid_frame().unwrap().clone();
    assert_eq!(initial.copies.len(), 2);
    let start = fixture.2.bodies[0].as_ref().unwrap().transform.pos[0];
    fixture.2.bodies[0].as_mut().unwrap().transform.pos[0] = start + 0.1;
    fixture.2.prototype.as_mut().unwrap().transform.pos[0] = start + 0.1;
    observe(&mut runtime, &fixture, 2.0 * TICK, 0.0, false);
    fixture.2.bodies[0].as_mut().unwrap().transform.pos[0] = start + 0.5;
    fixture.2.prototype.as_mut().unwrap().transform.pos[0] = start + 0.5;
    observe(&mut runtime, &fixture, 2.0 * TICK, 0.0, false);
    runtime.advance(true).unwrap();
    let historical = runtime.coupled_rigid_frame().unwrap();
    assert_eq!(historical.stamp.tick, 2);
    assert!((historical.poses[0].pos[0] - (start + 0.1)).abs() < 1e-5);
    for (copy, initial) in historical.copies.iter().zip(&initial.copies) {
        assert!((copy.pos[0] - (initial.pos[0] + 0.1)).abs() < 1e-5);
    }
    observe(&mut runtime, &fixture, 3.0 * TICK, 0.0, false);
    runtime.advance(true).unwrap();
    let edited = runtime.coupled_rigid_frame().unwrap();
    assert_eq!(edited.stamp.tick, 3);
    assert!((edited.poses[0].pos[0] - (start + 0.5)).abs() < 1e-5);
    for (copy, initial) in edited.copies.iter().zip(&initial.copies) {
        assert!((copy.pos[0] - (initial.pos[0] + 0.5)).abs() < 1e-5);
    }
}

#[test]
fn fluid_coupled_worker_inactive_fragments_do_not_displace_initial_liquid() {
    fn run(selected: bool) -> u32 {
        let mut fixture = fixture();
        fixture.2.acceleration_field = None;
        fixture.2.bodies[0].as_mut().unwrap().kind = 1;
        let mut fragment = fixture.2.bodies[0].clone().unwrap();
        fragment.fragment_parent = Some(0);
        fixture.2.bodies[1] = Some(fragment);
        let mut runtime = FluidRuntime::default();
        for time in [0.0, TICK] {
            runtime
                .observe_coupled_scene_with_field(
                    fixture.0,
                    fixture.1,
                    &[],
                    None,
                    Some(CoupledRigidInputs {
                        scene: &fixture.2,
                        colliders: RigidImpulseTargets {
                            bodies: if selected { 2 } else { 0 },
                            copies: false,
                        },
                        density: 1000.0,
                    }),
                    Seconds(time),
                    1.0,
                    0.0,
                )
                .unwrap();
            runtime.advance(true).unwrap();
        }
        runtime.stats.particles
    }
    let disabled = run(true);
    assert!(disabled > 0);
    assert_eq!(disabled, run(false));
}

#[test]
fn fluid_coupled_worker_mesh_growth_preserves_pair_and_impulse_receipts() {
    let mut fixture = fixture();
    fixture.0.max_vertices = 3;
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, true);
    runtime.advance(true).unwrap();
    let initial = runtime.coupled_rigid_frame().unwrap().clone();
    let version = runtime.version;
    enqueue_scene(&mut runtime, 1, 0.0, combined_target(), 0.25);
    observe(&mut runtime, &fixture, TICK, 0.0, true);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 1);
    assert!(runtime.version > version);
    assert_eq!(runtime.coupled_rigid_frame().unwrap().stamp.epoch, initial.stamp.epoch);
    assert_eq!(runtime.coupled_rigid_frame().unwrap().stamp.tick, 1);
    assert!(runtime.vertices.len() > fixture.0.max_vertices);
    let receipts: Vec<_> = runtime.drain_scene_impulses().collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].value.target, combined_target());
    assert_eq!(receipts[0].applied.tick, 0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 1);
    assert_eq!(runtime.drain_scene_impulses().count(), 0);
    observe(&mut runtime, &fixture, 2.0 * TICK, 0.0, true);
    runtime.advance(true).unwrap();
    assert_eq!(
        runtime.coupled_rigid_frame().unwrap().stamp,
        TickStamp {
            epoch: runtime.epoch,
            tick: 2
        }
    );
    assert!(!runtime.vertices.is_empty());
}

fn combined_target() -> ImpulseTarget {
    ImpulseTarget::FluidAndRigid(RigidImpulseTargets {
        bodies: 1,
        copies: false,
    })
}

fn enqueue_scene(
    runtime: &mut FluidRuntime,
    sequence: u64,
    time: f64,
    target: ImpulseTarget,
    strength: f32,
) -> TickStamp {
    runtime
        .enqueue_scene_impulse(
            EventStamp {
                epoch: runtime.epoch,
                time: Seconds(time),
                sequence,
            },
            ResolvedNodeImpulse {
                field: FieldValue::uniform([strength, 0.0, 0.0]).unwrap(),
                target,
            },
        )
        .unwrap()
}

fn empty_fixture() -> (FluidSettings, FluidControls, RigidSceneInputs) {
    let mut fixture = fixture();
    fixture.0.resolution = 8;
    fixture.0.initial_volume = None;
    fixture.2.acceleration_field = None;
    fixture
}

#[test]
fn fluid_coupled_events_preserve_time_order_and_move_only_selected_bodies_and_copies() {
    let mut fixture = empty_fixture();
    let mut other = fixture.2.bodies[0].clone().unwrap();
    other.transform.pos[2] += 3.0;
    fixture.2.bodies[1] = Some(other.clone());
    other.transform.pos[2] += 3.0;
    fixture.2.prototype = Some(other);
    fixture.2.copy_count = 2.0;
    fixture.2.copy_spacing = 4.0;
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, false);
    runtime.advance(true).unwrap();
    let initial = runtime.coupled_rigid_frame().unwrap().clone();
    let target = ImpulseTarget::Rigid(RigidImpulseTargets {
        bodies: 1,
        copies: true,
    });
    // Source order and delivery order are deliberately different. The native
    // owner must not re-enqueue and reject sequence 1 after consuming 2.
    enqueue_scene(&mut runtime, 1, TICK, target, 1.0);
    enqueue_scene(&mut runtime, 2, 0.0, target, 2.0);
    observe(&mut runtime, &fixture, 3.0 * TICK, 0.0, false);
    runtime.advance(true).unwrap();
    let frame = runtime.coupled_rigid_frame().unwrap();
    assert_eq!(frame.stamp.tick, 3);
    let expected = (8.0 * TICK) as f32;
    assert!((frame.poses[0].pos[0] - initial.poses[0].pos[0] - expected).abs() < 1e-5);
    assert_eq!(frame.poses[1], initial.poses[1]);
    for (actual, initial) in frame.copies.iter().zip(&initial.copies) {
        assert!((actual.pos[0] - initial.pos[0] - expected).abs() < 1e-5);
    }
    let receipts: Vec<_> = runtime.drain_scene_impulses().collect();
    assert_eq!(
        receipts
            .iter()
            .map(|event| event.source.sequence)
            .collect::<Vec<_>>(),
        [2, 1]
    );
    assert_eq!(
        receipts
            .iter()
            .map(|event| event.applied.tick)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert!(
        receipts
            .iter()
            .all(|event| event.lateness == Seconds::ZERO && event.value.target == target)
    );
    runtime.advance(true).unwrap();
    assert_eq!(runtime.drain_scene_impulses().count(), 0);
}

#[test]
fn fluid_coupled_combined_event_matches_separate_participants_with_one_receipt() {
    fn run(combined: bool) -> FluidRuntime {
        let mut fixture = fixture();
        fixture.2.acceleration_field = None;
        let mut runtime = FluidRuntime::default();
        for tick in 0..=1 {
            observe(&mut runtime, &fixture, tick as f64 * TICK, 0.0, true);
            runtime.advance(true).unwrap();
        }
        assert!(runtime.stats.particles > 0);
        if combined {
            enqueue_scene(&mut runtime, 1, TICK, combined_target(), 0.5);
        } else {
            enqueue_scene(&mut runtime, 1, TICK, ImpulseTarget::Fluid, 0.5);
            enqueue_scene(
                &mut runtime,
                2,
                TICK,
                ImpulseTarget::Rigid(RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                }),
                0.5,
            );
        }
        observe(&mut runtime, &fixture, 3.0 * TICK, 0.0, true);
        runtime.advance(true).unwrap();
        let receipts: Vec<_> = runtime.drain_scene_impulses().collect();
        assert_eq!(receipts.len(), if combined { 1 } else { 2 });
        assert!(receipts.iter().all(|event| event.applied.tick == 1));
        runtime
    }
    let combined = run(true);
    let separate = run(false);
    let a = combined.coupled_rigid_frame().unwrap();
    let b = separate.coupled_rigid_frame().unwrap();
    let initial = fixture().2.bodies[0].as_ref().unwrap().transform.pos[0];
    assert!(a.poses[0].pos[0] > initial + 0.005);
    for (actual, expected) in a.poses[0].pos.iter().zip(&b.poses[0].pos) {
        assert!((actual - expected).abs() < 1e-5);
    }
    assert_eq!(combined.vertices.len(), separate.vertices.len());
    for (actual, expected) in combined.vertices.iter().zip(&separate.vertices) {
        for (actual, expected) in actual.position.iter().zip(&expected.position) {
            assert!((actual - expected).abs() < 1e-5);
        }
    }
}

#[test]
fn fluid_coupled_event_late_arrival_waits_for_the_next_worker_batch() {
    let fixture = empty_fixture();
    let mut runtime = FluidRuntime::default();
    let (requests, replies) = mock_worker(&mut runtime);
    let mut native = NativeSimulation::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, false);
    runtime.advance(false).unwrap();
    replies
        .send(native.process(requests.recv().unwrap(), &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(false).unwrap();
    observe(&mut runtime, &fixture, 2.0 * TICK, 0.0, false);
    runtime.advance(false).unwrap();
    let first = requests.recv().unwrap();
    let target = combined_target();
    let assigned = enqueue_scene(&mut runtime, 1, 0.0, target, 1.0);
    assert_eq!(assigned.tick, 2);
    replies
        .send(native.process(first, &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(runtime.completed_tick, 2);
    let before = runtime.coupled_rigid_frame().unwrap().poses[0].pos[0];
    assert_eq!(runtime.drain_scene_impulses().count(), 0);
    observe(&mut runtime, &fixture, 3.0 * TICK, 0.0, false);
    runtime.advance(false).unwrap();
    replies
        .send(native.process(requests.recv().unwrap(), &runtime.cancel_epoch))
        .unwrap();
    runtime.advance(false).unwrap();
    let after = runtime.coupled_rigid_frame().unwrap().poses[0].pos[0];
    assert!((after - before - TICK as f32).abs() < 1e-5);
    let receipts: Vec<_> = runtime.drain_scene_impulses().collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].applied, assigned);
    assert_eq!(receipts[0].source.time, Seconds::ZERO);
    assert_eq!(receipts[0].lateness, Seconds(2.0 * TICK));
    assert_eq!(receipts[0].value.target, target);
}

#[test]
fn fluid_coupled_invalid_targets_preserve_sequence_and_partial_drains_preserve_receipts() {
    let fixture = empty_fixture();
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, &fixture, 0.0, 0.0, false);
    let stamp = EventStamp {
        epoch: runtime.epoch,
        time: Seconds::ZERO,
        sequence: 1,
    };
    for targets in [
        RigidImpulseTargets::default(),
        RigidImpulseTargets {
            bodies: 2,
            copies: false,
        },
        RigidImpulseTargets {
            bodies: 0,
            copies: true,
        },
    ] {
        assert!(
            runtime
                .enqueue_scene_impulse(
                    stamp,
                    ResolvedNodeImpulse {
                        field: FieldValue::uniform([0.0; 3]).unwrap(),
                        target: ImpulseTarget::FluidAndRigid(targets),
                    }
                )
                .is_err()
        );
        assert_eq!(runtime.impulse_outstanding, 0);
    }
    enqueue_scene(&mut runtime, 1, 0.0, combined_target(), 0.0);
    enqueue_scene(&mut runtime, 2, 0.0, ImpulseTarget::Fluid, 0.0);
    enqueue_scene(&mut runtime, 3, 0.0, ImpulseTarget::Fluid, 0.0);
    observe(&mut runtime, &fixture, TICK, 0.0, false);
    runtime.advance(true).unwrap();
    assert_eq!(
        runtime
            .drain_applied_impulses()
            .next()
            .unwrap()
            .source
            .sequence,
        2
    );
    assert_eq!(runtime.impulse_outstanding, 2);
    let receipts: Vec<_> = runtime.drain_scene_impulses().collect();
    assert_eq!(
        receipts
            .iter()
            .map(|event| event.source.sequence)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_eq!(runtime.impulse_outstanding, 0);

    // More than one native receipt buffer's worth over the lifetime of a world
    // must work: the shared worker drains its private receipts after each tick.
    for tick in 1..=130 {
        let time = tick as f64 * TICK;
        enqueue_scene(
            &mut runtime,
            (2 * tick + 2) as u64,
            time,
            combined_target(),
            0.0,
        );
        enqueue_scene(
            &mut runtime,
            (2 * tick + 3) as u64,
            time,
            combined_target(),
            0.0,
        );
        observe(&mut runtime, &fixture, time + TICK, 0.0, false);
        runtime.advance(true).unwrap();
        assert_eq!(runtime.drain_scene_impulses().count(), 2);
    }
    enqueue_scene(&mut runtime, 300, 131.0 * TICK, combined_target(), 1.0);
    observe(&mut runtime, &fixture, 131.0 * TICK, 1.0, false);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.impulse_outstanding, 0);
    assert_eq!(runtime.drain_scene_impulses().count(), 0);
}
