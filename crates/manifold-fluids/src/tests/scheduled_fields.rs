//! Numerical conformance of the shared event clock with both native backends.

use super::{field_world, marker_velocity};
use manifold_physics::input::{AppliedEvent, EventQueue, EventStamp};
use manifold_physics::{BodyConfig, FieldInput, PhysicsWorld, Seconds, TickStamp, UniformField};

const DT: Seconds = Seconds(1.0 / 60.0);
const EPOCH: u64 = 7;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Impulse {
    direction: [f32; 3],
    strength: f32,
}

struct Pair {
    fluid: crate::FluidWorld,
    rigid: PhysicsWorld,
    body: manifold_physics::BodyHandle,
}

impl Pair {
    fn new() -> Self {
        let mut rigid = PhysicsWorld::new([0.0; 3]).unwrap();
        let cube = [
            [-0.5, -0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, 0.5, 0.5],
            [0.5, -0.5, -0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, -0.5],
            [0.5, 0.5, 0.5],
        ];
        let body = rigid.add_hull(&cube, BodyConfig::default()).unwrap();
        Self {
            fluid: field_world(1),
            rigid,
            body,
        }
    }

    fn step(&mut self, impulses: &[Impulse]) -> ([f32; 3], [f32; 3]) {
        // Storage is bounded by this fixture's event count. Each solver gets
        // the same resolved field and delta velocity, once at the tick start.
        let fields: Vec<_> = impulses
            .iter()
            .map(|impulse| UniformField::new(impulse.direction).unwrap())
            .collect();
        let inputs: Vec<_> = impulses
            .iter()
            .zip(&fields)
            .map(|(impulse, field)| FieldInput {
                field,
                acceleration: 0.0,
                delta_velocity: impulse.strength,
            })
            .collect();
        self.rigid.apply_fields(&[self.body], &inputs, DT).unwrap();
        self.rigid.step(DT, 4).unwrap();
        self.fluid.step_with_fields(DT, &inputs).unwrap();
        (
            self.rigid.linear_velocity(self.body).unwrap(),
            marker_velocity(&mut self.fluid),
        )
    }
}

fn fixture() -> Vec<(EventStamp, Impulse)> {
    // Equal-time impulses add; a boundary event belongs to the following tick.
    [
        (0.2, [1.0, 0.0, 0.0], 0.25),
        (1.0, [1.0, 0.0, 0.0], 0.4),
        (1.0, [0.0, 0.0, 1.0], 0.2),
        (4.1, [-1.0, 0.0, 0.0], 0.1),
        (8.0, [0.0, 0.0, 1.0], 0.1),
    ]
    .into_iter()
    .enumerate()
    .map(|(sequence, (ticks, direction, strength))| {
        (
            EventStamp {
                epoch: EPOCH,
                time: Seconds(ticks * DT.0),
                sequence: sequence as u64,
            },
            Impulse {
                direction,
                strength,
            },
        )
    })
    .collect()
}

type NativeTrace = Vec<([f32; 3], [f32; 3])>;
type EventTrace = Vec<(u64, u64, f64)>;

fn simulate(delivery_boundaries: &[f64]) -> (NativeTrace, EventTrace) {
    let mut pair = Pair::new();
    let mut queue = EventQueue::new(EPOCH, Seconds::ZERO, DT, 16).unwrap();
    let inputs = fixture();
    let mut delivered = 0;
    let mut trace = Vec::new();
    let mut receipts = Vec::new();
    let mut impulses = Vec::with_capacity(inputs.len());
    for &boundary in delivery_boundaries {
        while delivered < inputs.len() && inputs[delivered].0.time.0 <= boundary {
            let (stamp, value) = inputs[delivered];
            queue.enqueue(stamp, value).unwrap();
            delivered += 1;
        }
        while (queue.next_tick().tick + 1) as f64 * DT.0 <= boundary {
            impulses.clear();
            queue
                .begin_tick(queue.next_tick(), |event: AppliedEvent<Impulse>| {
                    receipts.push((event.source.sequence, event.applied.tick, event.lateness.0));
                    impulses.push(event.value);
                })
                .unwrap();
            trace.push(pair.step(&impulses));
        }
    }
    assert_eq!(delivered, inputs.len());
    assert!(queue.is_empty());
    assert_eq!(trace.len(), 12);
    (trace, receipts)
}

#[test]
fn scheduled_fields_match_native_motion_at_24_30_60_fps_and_one_stalled_delivery() {
    let boundaries = |fps: f64| {
        let duration = 12.0 * DT.0;
        let mut times: Vec<_> = (1..=(duration * fps).floor() as usize)
            .map(|frame| frame as f64 / fps)
            .collect();
        if times.last().copied() != Some(duration) {
            times.push(duration);
        }
        times
    };
    let (expected, events) = simulate(&boundaries(60.0));
    assert_eq!(
        events,
        [
            (0, 0, 0.0),
            (1, 1, 0.0),
            (2, 1, 0.0),
            (3, 4, 0.0),
            (4, 8, 0.0)
        ]
    );
    assert!(
        expected.last().unwrap().0[0] > 0.5,
        "rigid receives the impulses"
    );
    assert!(
        expected.last().unwrap().1[0] > 0.2,
        "fluid receives the impulses"
    );
    for delivery in [boundaries(24.0), boundaries(30.0), vec![12.0 * DT.0]] {
        let (actual, receipts) = simulate(&delivery);
        assert_eq!(receipts, events);
        for (tick, ((rigid, fluid), (expected_rigid, expected_fluid))) in
            actual.iter().zip(&expected).enumerate()
        {
            for axis in 0..3 {
                assert!(
                    (rigid[axis] - expected_rigid[axis]).abs() < 1e-5,
                    "rigid tick {tick} axis {axis}: {rigid:?} vs {expected_rigid:?}"
                );
                assert!(
                    (fluid[axis] - expected_fluid[axis]).abs() < 1e-4,
                    "fluid tick {tick} axis {axis}: {fluid:?} vs {expected_fluid:?}"
                );
            }
        }
    }
}

#[test]
fn late_field_uses_next_tick_and_reset_cancels_unstarted_impulse() {
    let mut pair = Pair::new();
    let mut queue = EventQueue::new(EPOCH, Seconds::ZERO, DT, 4).unwrap();
    queue
        .begin_tick(queue.next_tick(), |_| panic!("no input yet"))
        .unwrap();
    let (rigid_before, fluid_before) = pair.step(&[]);
    let source_time = Seconds(DT.0 * 0.25);
    assert_eq!(
        queue
            .enqueue(
                EventStamp {
                    epoch: EPOCH,
                    time: source_time,
                    sequence: 1
                },
                Impulse {
                    direction: [1.0, 0.0, 0.0],
                    strength: 0.5
                }
            )
            .unwrap(),
        TickStamp {
            epoch: EPOCH,
            tick: 1
        }
    );
    let mut impulses = Vec::new();
    queue
        .begin_tick(queue.next_tick(), |event| {
            assert_eq!(event.source.time, source_time);
            assert_eq!(event.applied.tick, 1);
            assert!((event.lateness.0 - 0.75 * DT.0).abs() < 1e-12);
            impulses.push(event.value);
        })
        .unwrap();
    let (rigid_after, fluid_after) = pair.step(&impulses);
    assert!((rigid_after[0] - rigid_before[0] - 0.5).abs() < 1e-5);
    assert!(fluid_after[0] > fluid_before[0] + 0.25);

    queue
        .enqueue(
            EventStamp {
                epoch: EPOCH,
                time: Seconds(3.0 * DT.0),
                sequence: 2,
            },
            Impulse {
                direction: [1.0, 0.0, 0.0],
                strength: 100.0,
            },
        )
        .unwrap();
    assert_eq!(queue.reset(EPOCH + 1, Seconds(10.0)).unwrap(), 1);
    queue
        .begin_tick(queue.next_tick(), |_| {
            panic!("old epoch event survived reset")
        })
        .unwrap();
    let (rigid_empty, fluid_empty) = pair.step(&[]);
    assert!((rigid_empty[0] - rigid_after[0]).abs() < 1e-5);
    assert!((fluid_empty[0] - fluid_after[0]).abs() < 0.1);
}
