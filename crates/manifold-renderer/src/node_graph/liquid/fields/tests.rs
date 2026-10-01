use super::*;
use crate::node_graph::liquid::clock::LiquidClock;
use crate::node_graph::physics_events::ImpulseTarget;

fn fluid(field: FieldValue) -> ResolvedNodeImpulse {
    ResolvedNodeImpulse { field, target: ImpulseTarget::Fluid }
}

fn lattice() -> FieldLattice {
    FieldLattice::covering([-1.0, -0.5, -2.0], 0.125, [19, 11, 23])
}

/// One display frame at `transport`: advance the clock, then the impulses.
fn frame(clock: &mut LiquidClock, impulses: &mut LiquidImpulses, transport: f64, interval: f64, speed: f32) -> ClockFrame {
    let frame = clock.advance(transport, interval, speed, 0.0, false, false);
    impulses.observe_frame(transport, &frame).unwrap();
    frame
}

fn hit(impulses: &mut LiquidImpulses, transport: f64, sequence: u64, x: f32) -> TickStamp {
    let stamp = impulses.stamp(transport, sequence).unwrap();
    impulses
        .enqueue(stamp, fluid(FieldValue::uniform([x, 0.0, 0.0]).unwrap()), None)
        .unwrap()
}

fn receipts(impulses: &mut LiquidImpulses) -> Vec<AppliedEvent<ResolvedNodeImpulse>> {
    let mut out = Vec::new();
    impulses.drain_applied(&mut |event| out.push(event), None);
    out
}

/// A clock's first frame: a restart at time 0 that runs no tick.
fn still() -> ClockFrame {
    LiquidClock::default().advance(0.0, TICK, 1.0, 0.0, false, false)
}

/// Run a live clock at `frames_per_tick` ticks a frame under a field whose
/// strength moves linearly with transport, and collect each tick's lattice.
fn tick_lattices(frames_per_tick: u32, ticks: u64) -> Vec<Vec<[f32; 4]>> {
    let lattice = lattice();
    let mut clock = LiquidClock::default();
    let mut fields = LiquidFields::default();
    let impulses = LiquidImpulses::default();
    let interval = f64::from(frames_per_tick) * TICK;
    let mut out = Vec::new();
    let mut transport = 0.0;
    while (out.len() as u64) < ticks {
        let frame = clock.advance(transport, interval, 1.0, 0.0, false, false);
        // Dyadic values, so interpolating between frames is exact.
        let strength = 0.25 * (transport / TICK).round() as f32;
        let field = FieldValue::uniform([1.0, -2.0, 0.5]).unwrap().scaled(strength).unwrap();
        let field_frame = fields.prepare(lattice, Some(&field), &frame, &impulses).unwrap();
        if frame.ticks > 0 {
            assert_eq!(field_frame.force_lattices, frame.ticks, "a moving field gives each tick its lattice");
            assert_eq!(first_tick(&frame), out.len() as u64);
            out.extend(fields.forces().chunks_exact(lattice.node_count()).map(<[_]>::to_vec));
        }
        transport += interval;
    }
    out.truncate(ticks as usize);
    out
}

/// BUG-dzwl (per-tick force lattices): tick k reads the field at its own
/// start, so a moving field gives identical lattices at 60 and 30 fps.
#[test]
fn liquid_force_lattices_match_across_frame_rates() {
    let at_60 = tick_lattices(1, 12);
    let at_30 = tick_lattices(2, 12);
    for (tick, (a, b)) in at_60.iter().zip(&at_30).enumerate() {
        assert_eq!(a, b, "tick {tick}");
        let want = 0.25 * tick as f32;
        assert_eq!(a[0], [want, -2.0 * want, 0.5 * want, 0.0], "tick {tick} reads the field at its start");
    }
}

/// BUG-cykz (receipts only for ticks that ran): a frame whose fields fail
/// never commits, so its hit is discarded with a receipt, never "applied".
#[test]
fn liquid_impulse_failed_frame_discards() {
    let mut clock = LiquidClock::default();
    let mut impulses = LiquidImpulses::default();
    frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
    frame(&mut clock, &mut impulses, TICK, TICK, 1.0);
    let stamp = impulses.stamp(TICK, 1).unwrap();
    impulses
        .enqueue(stamp, fluid(FieldValue::uniform([1.0, 0.0, 0.0]).unwrap()), None)
        .unwrap();
    let next = frame(&mut clock, &mut impulses, 2.0 * TICK, TICK, 1.0);
    assert_eq!(impulses.impulse_tick(), Some(1), "the hit's tick began");
    let broken = FieldValue::uniform([f32::MAX, 0.0, 0.0]).unwrap().scaled(f32::MAX).unwrap();
    let error = LiquidFields::default().prepare(lattice(), Some(&broken), &next, &impulses).unwrap_err();
    assert!(error.contains("not finite"), "{error}");
    // The domain never reaches commit_frame; draining is the backstop.
    assert!(receipts(&mut impulses).is_empty());
    let mut discarded = Vec::new();
    impulses.drain_discarded(&mut |stamp| discarded.push(stamp));
    assert_eq!(discarded, vec![stamp]);
    // A later good frame neither applies nor replays it.
    frame(&mut clock, &mut impulses, 3.0 * TICK, TICK, 1.0);
    assert_eq!(impulses.impulse_tick(), None);
    impulses.commit_frame();
    assert!(receipts(&mut impulses).is_empty());
}

#[test]
fn liquid_field_lattice_covers_the_solver_lattice() {
    let lattice = lattice();
    assert_eq!(lattice.nodes(), [6, 4, 7]);
    assert_eq!(lattice.spacing(), 0.5);
    assert_eq!(lattice.bytes(), 6 * 4 * 7 * 16);
    // The field lattice reaches past the solver lattice's last node.
    let solver_extent = [18.0 * 0.125, 10.0 * 0.125, 22.0 * 0.125];
    for (nodes, extent) in lattice.nodes().into_iter().zip(solver_extent) {
        assert!((nodes - 1) as f32 * lattice.spacing() >= extent);
    }
}

/// The force lattice holds the field's value at each node, and the shader's
/// CPU twin interpolates between them.
#[test]
fn liquid_force_lattice_matches_field() {
    let field = FieldValue::radial([0.2, 0.1, -0.4], 1.5, 1.0)
        .unwrap()
        .sum(&FieldValue::vortex([0.0, 0.0, 0.0], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap())
        .unwrap()
        .sum(&FieldValue::uniform([0.0, -3.0, 0.5]).unwrap())
        .unwrap();
    let lattice = lattice();
    let mut fields = LiquidFields::default();
    let still = still();
    let frame = fields.prepare(lattice, Some(&field), &still, &LiquidImpulses::default()).unwrap();
    assert_eq!(frame.force_lattices, 1);
    assert_eq!(frame.impulse_tick, None);
    let [nx, ny, nz] = lattice.nodes().map(|n| n as usize);
    for z in 0..nz {
        for y in 0..ny {
            for x in 0..nx {
                let index = x + nx * (y + ny * z);
                let position = [-1.0 + x as f32 * 0.5, -0.5 + y as f32 * 0.5, -2.0 + z as f32 * 0.5];
                let expected = field.sample(position);
                assert_eq!(fields.forces()[index], [expected[0], expected[1], expected[2], 0.0]);
                // At a node the trilinear read returns the node.
                let read = lattice.sample(fields.forces(), position);
                for axis in 0..3 {
                    assert!((read[axis] - expected[axis]).abs() <= 1e-5 * (1.0 + expected[axis].abs()));
                }
            }
        }
    }
    // Midway between two nodes the read is their mean; past the last node it
    // holds the last node.
    let a = fields.forces()[1 + nx];
    let b = fields.forces()[2 + nx];
    let mid = lattice.sample(fields.forces(), [-1.0 + 0.75, 0.0, -2.0]);
    for axis in 0..3 {
        assert!((mid[axis] - 0.5 * (a[axis] + b[axis])).abs() < 1e-5);
    }
    let last = fields.forces()[nx * ny * nz - 1];
    let beyond = lattice.sample(fields.forces(), [10.0, 10.0, 10.0]);
    for axis in 0..3 {
        assert!((beyond[axis] - last[axis]).abs() < 1e-5);
    }
    // An unchanged field is not resampled; no field turns forces off.
    fields.forces_dirty = false;
    fields.prepare(lattice, Some(&field), &still, &LiquidImpulses::default()).unwrap();
    assert!(!fields.forces_dirty);
    let off = fields.prepare(lattice, None, &still, &LiquidImpulses::default()).unwrap();
    assert_eq!(off.force_lattices, 0);
    assert!(fields.forces().is_empty());
}

#[test]
fn liquid_force_lattice_refuses_a_non_finite_field() {
    let field = FieldValue::uniform([f32::MAX, 0.0, 0.0]).unwrap().scaled(f32::MAX).unwrap();
    let error = LiquidFields::default()
        .prepare(lattice(), Some(&field), &still(), &LiquidImpulses::default())
        .unwrap_err();
    assert!(error.contains("not finite"), "{error}");
}

/// A hit fired after a frame lands on the next frame's first tick, once,
/// however many ticks and substeps that frame runs.
#[test]
fn liquid_impulse_once_per_tick_across_substeps() {
    let mut clock = LiquidClock::default();
    let mut impulses = LiquidImpulses::default();
    frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
    frame(&mut clock, &mut impulses, TICK, TICK, 1.0);
    let planned = hit(&mut impulses, TICK, 1, 2.0);
    assert_eq!(planned.tick, 1);
    // A 30 fps frame runs two ticks; the hit lands on the first.
    let next = frame(&mut clock, &mut impulses, 3.0 * TICK, 2.0 * TICK, 1.0);
    assert_eq!(next.ticks, 2);
    assert_eq!(impulses.impulse_tick(), Some(1));
    let mut fields = LiquidFields::default();
    let lattice = lattice();
    let field_frame = fields.prepare(lattice, None, &next, &impulses).unwrap();
    impulses.commit_frame();
    assert_eq!(field_frame.impulse_tick, Some(1));
    assert!(fields.impulses().iter().all(|v| *v == [2.0, 0.0, 0.0, 0.0]));
    // The atoms' gate: the impulse tick's first substep, and nothing else.
    let substeps = 34;
    let applied: u32 = (1..3u64)
        .flat_map(|tick| (0..substeps).map(move |substep| (tick, substep)))
        .map(|(tick, substep)| u32::from(Some(tick) == field_frame.impulse_tick && substep == 0))
        .sum();
    assert_eq!(applied, 1);
    let delivered = receipts(&mut impulses);
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].applied.tick, 1);
    assert_eq!(delivered[0].lateness, Seconds(0.0));
    // The following frame carries no impulse and reuses no receipt.
    let after = frame(&mut clock, &mut impulses, 4.0 * TICK, TICK, 1.0);
    assert_eq!(impulses.impulse_tick(), None);
    assert_eq!(fields.prepare(lattice, None, &after, &impulses).unwrap().impulse_tick, None);
    impulses.commit_frame();
    assert!(receipts(&mut impulses).is_empty());
}

/// A coupled hold or a frame with no tick due keeps the hit; it lands on the
/// next tick that runs.
#[test]
fn liquid_impulse_waits_for_the_next_tick() {
    let mut clock = LiquidClock::default();
    let mut impulses = LiquidImpulses::default();
    frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
    clock.set_tick_cap(Some(0));
    let held = frame(&mut clock, &mut impulses, TICK, TICK, 1.0);
    assert!(!held.held && held.ticks == 0);
    hit(&mut impulses, TICK, 1, 1.0);
    clock.set_tick_cap(Some(1));
    frame(&mut clock, &mut impulses, 2.0 * TICK, TICK, 1.0);
    assert_eq!(impulses.impulse_tick(), Some(0));
    impulses.commit_frame();
    assert_eq!(receipts(&mut impulses).len(), 1);
}

/// Pause and Speed 0 discard hits with a receipt; resume never replays them.
#[test]
fn liquid_impulse_held_clock_discards() {
    for (transport, speed) in [(TICK, 1.0), (2.0 * TICK, 0.0)] {
        let mut clock = LiquidClock::default();
        let mut impulses = LiquidImpulses::default();
        frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
        frame(&mut clock, &mut impulses, TICK, TICK, 1.0);
        let paused = frame(&mut clock, &mut impulses, transport, TICK, speed);
        assert!(paused.held);
        let stamp = impulses.stamp(transport, 1).unwrap();
        impulses
            .enqueue(stamp, fluid(FieldValue::uniform([1.0, 0.0, 0.0]).unwrap()), None)
            .unwrap();
        let mut discarded = Vec::new();
        impulses.drain_discarded(&mut |stamp| discarded.push(stamp));
        assert_eq!(discarded, vec![stamp]);
        for i in 1..4 {
            frame(&mut clock, &mut impulses, transport + i as f64 * TICK, TICK, 1.0);
            assert_eq!(impulses.impulse_tick(), None);
        }
        assert!(receipts(&mut impulses).is_empty());
    }
}

/// A restart opens a new epoch: queued hits are cancelled and old stamps
/// are refused.
#[test]
fn liquid_impulse_restart_cancels() {
    let mut clock = LiquidClock::default();
    let mut impulses = LiquidImpulses::default();
    frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
    assert_eq!(impulses.epoch(), Some(1));
    let stamp = impulses.stamp(0.0, 1).unwrap();
    impulses
        .enqueue(stamp, fluid(FieldValue::uniform([1.0; 3]).unwrap()), None)
        .unwrap();
    let restart = clock.advance(TICK, TICK, 1.0, 1.0, false, false);
    impulses.observe_frame(TICK, &restart).unwrap();
    assert_eq!(impulses.epoch(), Some(2));
    let error = impulses
        .enqueue(EventStamp { sequence: 2, ..stamp }, fluid(FieldValue::uniform([1.0; 3]).unwrap()), None)
        .unwrap_err();
    assert!(error.contains("restarted"), "{error}");
    for i in 2..5 {
        let frame = clock.advance(i as f64 * TICK, TICK, 1.0, 1.0, false, false);
        impulses.observe_frame(i as f64 * TICK, &frame).unwrap();
        assert_eq!(impulses.impulse_tick(), None);
    }
    assert!(receipts(&mut impulses).is_empty());
}

#[test]
fn liquid_impulse_refusals_are_named() {
    let mut clock = LiquidClock::default();
    let mut impulses = LiquidImpulses::default();
    assert!(impulses.stamp(0.0, 0).unwrap_err().contains("not started"));
    frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
    assert!(impulses.stamp(0.5, 0).unwrap_err().contains("last ran"));
    let stamp = impulses.stamp(0.0, 0).unwrap();
    let rigid = ResolvedNodeImpulse {
        field: FieldValue::uniform([1.0; 3]).unwrap(),
        target: ImpulseTarget::Rigid(crate::node_graph::physics::RigidImpulseTargets { bodies: 1, copies: false }),
    };
    assert!(impulses.enqueue(stamp, rigid, None).unwrap_err().contains("coupled rigid world"));
    // The history is bounded and the overflow latches until a restart.
    for sequence in 0..IMPULSE_CAPACITY as u64 {
        hit(&mut impulses, 0.0, sequence, 1.0);
    }
    let stamp = impulses.stamp(0.0, 1000);
    let error = impulses
        .enqueue(stamp.unwrap(), fluid(FieldValue::uniform([1.0; 3]).unwrap()), None)
        .unwrap_err();
    assert!(error.contains("full"), "{error}");
    assert!(impulses.stamp(0.0, 1001).unwrap_err().contains("full"));
    let restart = clock.advance(TICK, TICK, 1.0, 1.0, false, false);
    impulses.observe_frame(TICK, &restart).unwrap();
    assert!(impulses.stamp(TICK, 0).is_ok());
}
