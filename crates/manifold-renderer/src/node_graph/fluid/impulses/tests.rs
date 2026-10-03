use super::*;
use crate::node_graph::fluid::{
    CacheMode, FluidControls, FluidSettings, FrameStats, Reply, Request, Transform, Worker,
    cancelled_reply,
};
use crate::node_graph::physics::PhysicsAuthoredSampleScope;
use std::sync::{Arc, mpsc};

fn empty_settings() -> FluidSettings {
    FluidSettings {
        resolution: 8,
        fill_height: 0.0,
        ..FluidSettings::default()
    }
}

fn controls() -> FluidControls {
    FluidControls {
        gravity: [0.0; 3],
        emission: false,
        obstacle_enabled: false,
        ..FluidControls::default()
    }
}

fn observe(runtime: &mut FluidRuntime, time: f64, reset: f32) {
    runtime
        .observe(empty_settings(), controls(), Seconds(time), 1.0, reset)
        .unwrap();
}

fn enqueue(runtime: &mut FluidRuntime, sequence: u64, time: f64, x: f32) -> TickStamp {
    runtime
        .enqueue_impulse(
            EventStamp {
                epoch: runtime.impulse_epoch().unwrap(),
                time: Seconds(time),
                sequence,
            },
            FieldValue::uniform([x, 0.0, 0.0]).unwrap(),
        )
        .unwrap()
}

#[test]
fn fluid_impulse_stamp_tracks_target_time_from_exact_transport() {
    let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
    let mut runtime = FluidRuntime::default();
    assert!(
        runtime
            .impulse_stamp(Seconds::ZERO, 0)
            .expect_err("stamp before observation")
            .contains("epoch")
    );
    observe(&mut runtime, 5.0, 0.0);
    let epoch = runtime.impulse_epoch().unwrap();
    assert_eq!(
        runtime.impulse_stamp(Seconds(5.0), 1).unwrap(),
        EventStamp {
            epoch,
            time: Seconds::ZERO,
            sequence: 1,
        }
    );
    runtime
        .observe(empty_settings(), controls(), Seconds(6.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        runtime.impulse_stamp(Seconds(6.0), 2).unwrap().time,
        Seconds(1.0)
    );
    runtime
        .observe(empty_settings(), controls(), Seconds(6.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        runtime.impulse_stamp(Seconds(6.0), 3).unwrap().time,
        Seconds(1.0)
    );
    runtime
        .observe(empty_settings(), controls(), Seconds(7.0), 2.0, 0.0)
        .unwrap();
    assert_eq!(
        runtime.impulse_stamp(Seconds(7.0), 4).unwrap().time,
        Seconds(3.0)
    );
    assert!(runtime
        .impulse_stamp(Seconds(7.0 + 1e-9), 5)
        .is_err());
}

#[test]
fn fluid_impulse_stamp_rejects_withheld_authored_settings_and_recovers_in_live_setup() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    let changed = FluidSettings {
        resolution: 10,
        ..empty_settings()
    };
    {
        let _scope = PhysicsAuthoredSampleScope::new();
        runtime
            .observe_scene(changed, controls(), &[], Seconds(TICK), 1.0, 0.0)
            .unwrap();
    }
    assert!(runtime.impulse_stamp(Seconds(TICK), 1).is_err());

    runtime
        .observe_scene(changed, controls(), &[], Seconds(TICK), 1.0, 0.0)
        .unwrap();
    let stamp = runtime.impulse_stamp(Seconds(TICK), 2).unwrap();
    assert_eq!(stamp.time, Seconds::ZERO);

    runtime.hold_pending(Seconds(2.0 * TICK));
    assert!(runtime.impulse_stamp(Seconds(2.0 * TICK), 3).is_err());
}

#[test]
fn fluid_impulses_multiple_hits_and_half_open_boundary_run_once() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    let first = enqueue(&mut runtime, 1, TICK * 0.2, 1.0);
    enqueue(&mut runtime, 2, TICK * 0.2, 2.0);
    let boundary = enqueue(&mut runtime, 3, TICK, 3.0);
    assert_eq!((first.tick, boundary.tick), (0, 1));
    observe(&mut runtime, TICK, 0.0);
    runtime.advance(true).unwrap();
    let receipts: Vec<_> = runtime.drain_applied_impulses().collect();
    assert_eq!(
        receipts
            .iter()
            .map(|e| e.source.sequence)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert!(
        receipts
            .iter()
            .all(|e| e.applied.tick == 0 && e.lateness == Seconds::ZERO)
    );
    observe(&mut runtime, TICK * 2.0, 0.0);
    runtime.advance(true).unwrap();
    let receipts: Vec<_> = runtime.drain_applied_impulses().collect();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].applied, boundary);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.drain_applied_impulses().count(), 0);
}

#[test]
fn fluid_impulses_pause_keeps_hits_until_the_first_tick() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    enqueue(&mut runtime, 1, 0.0, 1.0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.completed_tick, 0);
    assert_eq!(runtime.drain_applied_impulses().count(), 0);
    assert_eq!(runtime.impulses.len(), 1);
    observe(&mut runtime, TICK, 0.0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.drain_applied_impulses().count(), 1);
}

#[test]
fn fluid_impulses_known_input_delivery_matches_24_30_60_and_stalled_frames() {
    fn run(frame_times: impl Iterator<Item = f64>) -> Vec<(u64, u64, Seconds)> {
        let mut runtime = FluidRuntime::default();
        observe(&mut runtime, 0.0, 0.0);
        for (sequence, time) in [0.0, 0.004, 0.008, TICK, 0.7].into_iter().enumerate() {
            enqueue(&mut runtime, sequence as u64, time, 0.0);
        }
        let mut trace = Vec::new();
        for time in frame_times {
            observe(&mut runtime, time, 0.0);
            runtime.advance(true).unwrap();
            trace.extend(
                runtime
                    .drain_applied_impulses()
                    .map(|event| (event.source.sequence, event.applied.tick, event.lateness)),
            );
        }
        assert_eq!(runtime.completed_tick, 60);
        trace
    }
    let reference = run((1..=60).map(|frame| frame as f64 / 60.0));
    assert_eq!(reference.len(), 5);
    for fps in [24, 30] {
        assert_eq!(
            run((1..=fps).map(|frame| frame as f64 / fps as f64)),
            reference
        );
    }
    assert_eq!(run([0.5, 1.0].into_iter()), reference);
}

fn install_mock(runtime: &mut FluidRuntime) -> (mpsc::Receiver<Request>, mpsc::SyncSender<Reply>) {
    let (requests, receiver) = mpsc::sync_channel(1);
    let (sender, replies) = mpsc::sync_channel(1);
    runtime.worker = Some(Worker {
        requests,
        replies,
        cancel_epoch: Arc::clone(&runtime.cancel_epoch),
    });
    (receiver, sender)
}

fn completed(request: Request) -> Reply {
    let tick = request.start_tick + request.count as u64;
    let mut reply = cancelled_reply(request);
    reply.tick = tick;
    reply.started_tick = tick;
    reply
}

#[test]
fn fluid_impulses_busy_handoff_seals_ticks_and_reports_late_delivery() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    let (requests, replies) = install_mock(&mut runtime);
    runtime.advance(false).unwrap();
    replies.send(completed(requests.recv().unwrap())).unwrap();
    runtime.advance(false).unwrap();
    observe(&mut runtime, TICK * 4.0, 0.0);
    runtime.advance(false).unwrap();
    let busy = requests.recv().unwrap();
    assert_eq!(busy.count, 1);
    let planned = enqueue(&mut runtime, 1, TICK * 0.5, 1.0);
    assert_eq!(planned.tick, 1, "cannot rewrite the worker-owned interval");
    assert_eq!(runtime.drain_applied_impulses().count(), 0);
    replies.send(completed(busy)).unwrap();
    runtime.advance(false).unwrap();
    observe(&mut runtime, TICK * 5.0, 0.0);
    runtime.advance(false).unwrap();
    let next = requests.recv().unwrap();
    assert_eq!(next.impulses.len(), 1);
    assert_eq!(next.impulses[0].applied, planned);
    assert_eq!(
        runtime.drain_applied_impulses().count(),
        0,
        "handoff alone is not a native receipt"
    );
    replies.send(completed(next)).unwrap();
    runtime.advance(false).unwrap();
    let receipt = runtime.drain_applied_impulses().next().unwrap();
    assert_eq!(receipt.applied, planned);
    assert_eq!(receipt.lateness, Seconds(0.5 * TICK));
}

#[test]
fn fluid_impulses_reset_discards_stale_worker_receipts_and_pending_hits() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    let old_epoch = runtime.impulse_epoch().unwrap();
    enqueue(&mut runtime, 1, 0.0, 1.0);
    enqueue(&mut runtime, 2, 10.0, 1.0);
    let (requests, replies) = install_mock(&mut runtime);
    runtime.advance(false).unwrap();
    replies.send(completed(requests.recv().unwrap())).unwrap();
    runtime.advance(false).unwrap();
    observe(&mut runtime, TICK, 0.0);
    runtime.advance(false).unwrap();
    let old = requests.recv().unwrap();
    observe(&mut runtime, TICK, 1.0);
    assert_ne!(runtime.impulse_epoch(), Some(old_epoch));
    assert_eq!(runtime.impulse_outstanding, 0);
    replies.send(completed(old)).unwrap();
    runtime.advance(false).unwrap();
    assert_eq!(runtime.completed_tick, 0);
    assert_eq!(runtime.drain_applied_impulses().count(), 0);
    let new = requests.recv().unwrap();
    assert!(new.impulses.is_empty());
    replies.send(completed(new)).unwrap();
    runtime.advance(false).unwrap();
    let stale = runtime
        .enqueue_impulse(
            EventStamp {
                epoch: old_epoch,
                time: Seconds::ZERO,
                sequence: 1,
            },
            FieldValue::uniform([1.0; 3]).unwrap(),
        )
        .unwrap_err();
    assert!(stale.contains("different simulation epoch"));
    assert_eq!(enqueue(&mut runtime, 1, 0.0, 1.0).tick, 0);
}

#[test]
fn fluid_impulses_capacity_includes_undrained_receipts_and_is_resettable() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    let epoch = runtime.impulse_epoch().unwrap();
    for sequence in 0..IMPULSE_CAPACITY as u64 {
        enqueue(&mut runtime, sequence, 0.0, 0.0);
    }
    observe(&mut runtime, TICK, 0.0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.applied_impulses.len(), IMPULSE_CAPACITY);
    assert_eq!(runtime.applied_impulses.capacity(), IMPULSE_CAPACITY);
    assert_eq!(
        runtime.spare_impulses.as_ref().unwrap().capacity(),
        IMPULSE_CAPACITY
    );
    let stamp = EventStamp {
        epoch,
        time: Seconds(TICK),
        sequence: IMPULSE_CAPACITY as u64,
    };
    let field = FieldValue::uniform([0.0; 3]).unwrap();
    assert!(
        runtime
            .enqueue_impulse(stamp, field.clone())
            .unwrap_err()
            .contains("history is full")
    );
    assert_eq!(runtime.drain_applied_impulses().count(), IMPULSE_CAPACITY);
    assert!(
        runtime.advance(true).is_err(),
        "draining does not clear a latched overflow"
    );
    observe(&mut runtime, TICK, 1.0);
    assert_eq!(enqueue(&mut runtime, 1, 0.0, 0.0).tick, 0);
    assert!(runtime.enqueue_impulse(stamp, field).is_err());
}

#[test]
fn fluid_impulses_queue_overflow_preserves_unread_prefix() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    for sequence in 0..IMPULSE_CAPACITY as u64 {
        enqueue(&mut runtime, sequence, 10.0, 0.0);
    }
    let stamp = EventStamp {
        epoch: runtime.epoch,
        time: Seconds::ZERO,
        sequence: 256,
    };
    assert!(
        runtime
            .enqueue_impulse(stamp, FieldValue::uniform([0.0; 3]).unwrap())
            .is_err()
    );
    assert_eq!(runtime.impulses.len(), IMPULSE_CAPACITY);
    assert!(runtime.advance(true).is_err());
    assert_eq!(runtime.completed_tick, 0);
}

#[test]
fn fluid_impulses_native_failure_retains_receipts_and_cannot_retry_a_hit() {
    let mut runtime = FluidRuntime::default();
    observe(&mut runtime, 0.0, 0.0);
    enqueue(&mut runtime, 1, 0.0, f32::MAX);
    enqueue(&mut runtime, 2, 0.0, f32::MAX);
    observe(&mut runtime, TICK, 0.0);
    let error = runtime.advance(true).unwrap_err();
    assert_eq!(runtime.completed_tick, 0);
    assert_eq!(runtime.applied_impulses.len(), 2);
    assert!(
        runtime
            .applied_impulses
            .iter()
            .all(|event| event.applied.tick == 0)
    );
    assert_eq!(runtime.advance(true).unwrap_err(), error);
    assert_eq!(runtime.drain_applied_impulses().count(), 2);
    observe(&mut runtime, TICK, 1.0);
    runtime.advance(true).unwrap();
    assert_eq!(runtime.stats, FrameStats::default());
}

#[test]
fn fluid_impulses_legacy_cache_modes_cannot_claim_recorded_inputs() {
    for mode in [CacheMode::Record, CacheMode::Playback] {
        let mut runtime = FluidRuntime::default();
        runtime.set_cache(mode, "not-opened-by-observe").unwrap();
        observe(&mut runtime, 0.0, 0.0);
        let stamp = EventStamp {
            epoch: runtime.epoch,
            time: Seconds::ZERO,
            sequence: 0,
        };
        assert!(
            runtime
                .enqueue_impulse(stamp, FieldValue::uniform([1.0; 3]).unwrap())
                .unwrap_err()
                .contains("input takes")
        );
        assert_eq!(runtime.impulse_outstanding, 0);
    }
}

#[test]
fn fluid_impulses_sum_uses_scene_coordinates_and_owned_fields() {
    let center = [10.0, -3.0, 7.0];
    let field = FieldValue::radial(center, 4.0, 1.0).unwrap();
    let events = [AppliedEvent {
        source: EventStamp {
            epoch: 1,
            time: Seconds::ZERO,
            sequence: 1,
        },
        applied: TickStamp { epoch: 1, tick: 0 },
        lateness: Seconds::ZERO,
        value: ResolvedNodeImpulse {
            field,
            target: ImpulseTarget::Fluid,
        },
    }];
    let sum = ImpulseSum {
        events: &events,
        origin: [8.0, -5.0, 5.0],
    };
    assert_eq!(sum.sample([3.0, 2.0, 2.0]), [0.75, 0.0, 0.0]);
}

#[test]
fn fluid_impulses_move_native_liquid_once_across_substeps_and_batches() {
    fn run(impulse: bool, accelerated: bool, batched: bool) -> f32 {
        let settings = FluidSettings {
            resolution: 12,
            fill_height: 0.0,
            initial_volume: Some(Transform {
                pos: [0.0, 2.0, 0.0],
                scale: [1.5; 3],
                ..Transform::default()
            }),
            time_steps: manifold_fluids::TimeStepOptions {
                min_substeps: 4,
                max_substeps: 4,
                ..Default::default()
            },
            ..FluidSettings::default()
        };
        let mut runtime = FluidRuntime::default();
        let field = accelerated.then(|| FieldValue::uniform([0.0, -60.0, 0.0]).unwrap());
        runtime
            .observe_scene_with_field(settings, controls(), &[], None, Seconds::ZERO, 1.0, 0.0)
            .unwrap();
        // Native initial volumes seed at the end of their first substep.
        // Exercise a hit on existing liquid, after that initialization frame.
        runtime.observe_scene_with_field(settings, controls(), &[], field, Seconds(TICK), 1.0, 0.0).unwrap();
        runtime.advance(true).unwrap();
        if impulse {
            let epoch = runtime.impulse_epoch().unwrap();
            for (sequence, strength) in [(1, -0.25), (2, -0.75)] {
                runtime
                    .enqueue_impulse(
                        EventStamp {
                            epoch,
                            time: Seconds(TICK * 1.25),
                            sequence,
                        },
                        FieldValue::uniform([0.0, strength, 0.0]).unwrap(),
                    )
                    .unwrap();
            }
        }
        // The acceleration reference lasts exactly one outer tick. Subsequent
        // motion is free, so repeated impulses/substep multiplication diverge.
        runtime
            .observe_scene_with_field(settings, controls(), &[], None, Seconds(2.0 * TICK), 1.0, 0.0)
            .unwrap();
        if !batched {
            runtime.advance(true).unwrap();
        }
        for tick in 3..=7 {
            runtime
                .observe(settings, controls(), Seconds(tick as f64 * TICK), 1.0, 0.0)
                .unwrap();
            if !batched {
                runtime.advance(true).unwrap();
            }
        }
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, 7);
        assert!(!runtime.vertices.is_empty());
        assert_eq!(
            runtime.drain_applied_impulses().count(),
            if impulse { 2 } else { 0 }
        );
        runtime
            .vertices
            .iter()
            .map(|vertex| vertex.position[1])
            .sum::<f32>()
            / runtime.vertices.len() as f32
    }
    let resting = run(false, false, true);
    let reference = run(false, true, true);
    let impulse = run(true, false, true);
    let partitioned = run(true, false, false);
    assert!(
        resting - impulse > 0.01,
        "liquid must move: rest={resting}, impulse={impulse}"
    );
    assert!(
        (impulse - reference).abs() < 0.005,
        "impulse={impulse}, reference={reference}"
    );
    assert!(
        (impulse - partitioned).abs() < 0.005,
        "batched={impulse}, partitioned={partitioned}"
    );
}
