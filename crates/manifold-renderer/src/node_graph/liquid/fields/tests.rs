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

/// One GPU liquid driven as the host drives it. Before each frame the physics
/// history replay asks for the ticks' starts and evaluates the field's
/// authored ancestry there (here a function of transport time), then closes
/// the interval at the frame's own time; the frame then advances the clock
/// and lays out its ticks' fields.
struct Rig {
    clock: LiquidClock,
    fields: LiquidFields,
    impulses: LiquidImpulses,
    last: Option<f64>,
    times: Vec<f64>,
}

impl Rig {
    fn new() -> Self {
        Self {
            clock: LiquidClock::default(),
            fields: LiquidFields::default(),
            impulses: LiquidImpulses::default(),
            last: None,
            times: Vec::new(),
        }
    }

    fn frame(
        &mut self,
        transport: f64,
        interval: f64,
        speed: f32,
        offline: bool,
        field: &dyn Fn(f64) -> FieldValue,
    ) -> Result<(ClockFrame, FieldFrame), String> {
        if let Some(last) = self.last.filter(|&last| transport > last) {
            self.times.clear();
            self.fields.request_samples(&self.clock, last, transport, &mut self.times);
            for &time in self.times.iter().filter(|&&time| time < transport) {
                self.fields.observe_sample(time, Some(&field(time)));
            }
            self.fields.observe_sample(transport, Some(&field(transport)));
        }
        self.last = Some(transport);
        let frame = self.clock.advance(transport, interval, speed, 0.0, false, offline);
        self.impulses.observe_frame(transport, &frame)?;
        let laid = self.fields.prepare(lattice(), Some(&field(transport)), &self.clock, &frame, &self.impulses)?;
        Ok((frame, laid))
    }

    /// This frame's force lattice for each tick it ran.
    fn tick_lattices(&self, frame: &ClockFrame, laid: &FieldFrame) -> Vec<Vec<[f32; 4]>> {
        let count = lattice().node_count();
        let forces = self.fields.forces();
        (0..frame.ticks as usize)
            .map(|tick| {
                let index = if laid.force_lattices == 1 { 0 } else { tick };
                forces[index * count..(index + 1) * count].to_vec()
            })
            .collect()
    }
}

/// A curved control (an LFO) plus a sharp kick at 0.4 s that decays fast:
/// the shape a frame-held or interpolated field gets wrong between frames.
fn modulated(time: f64) -> FieldValue {
    let lfo = (std::f64::consts::TAU * 1.7 * time).sin() as f32;
    let kick = if time >= 0.4 { (6.0 * (-(time - 0.4) * 30.0).exp()) as f32 } else { 0.0 };
    FieldValue::uniform([lfo + kick, -2.0 * lfo, 0.5]).unwrap()
}

fn expected(time: f64) -> Vec<[f32; 4]> {
    let mut values = vec![[0.0; 4]; lattice().node_count()];
    fill(&lattice(), &mut values, |x| modulated(time).sample(x)).unwrap();
    values
}

/// Every tick's lattice over `seconds` at `fps`, with each tick's index.
fn run_at(fps: f64, speed: f32, offline: bool, seconds: f64) -> Vec<(u64, Vec<[f32; 4]>)> {
    let mut rig = Rig::new();
    let mut out = Vec::new();
    let frames = (seconds * fps).round() as u64;
    for index in 0..=frames {
        let transport = index as f64 / fps;
        let (frame, laid) = rig.frame(transport, 1.0 / fps, speed, offline, &modulated).unwrap();
        assert_eq!(frame.dropped_seconds, 0.0, "{fps} fps dropped time");
        let first = first_tick(&frame);
        assert_eq!(first, out.len() as u64, "ticks run in order, none skipped or merged");
        for (offset, lattice) in rig.tick_lattices(&frame, &laid).into_iter().enumerate() {
            out.push((first + offset as u64, lattice));
        }
    }
    out
}

/// BUG-8tl0 (liquid forces per tick): every tick reads the field at its own
/// start, evaluated there, so a curved control and a kick between frames
/// give the same per-tick forces at 24, 30 and 60 fps, live and exported.
#[test]
fn liquid_forces_per_tick_match_across_frame_rates() {
    for offline in [false, true] {
        let reference = run_at(60.0, 1.0, offline, 1.0);
        assert!(reference.len() >= 59);
        for &(tick, ref lattice) in &reference {
            assert_eq!(*lattice, expected(tick as f64 * TICK), "tick {tick} reads its own start");
        }
        for fps in [24.0, 30.0] {
            let run = run_at(fps, 1.0, offline, 1.0);
            let shared = run.len().min(reference.len());
            assert!(shared >= 58, "{fps} fps ran {shared} ticks");
            assert_eq!(run[..shared], reference[..shared], "{fps} fps, offline {offline}");
        }
    }
}

/// At Speed 0.5 tick k starts at transport k·TICK / 0.5: its force is the
/// field at that time, the same at every frame rate.
#[test]
fn liquid_forces_per_tick_follow_simulation_speed() {
    let reference = run_at(60.0, 0.5, false, 1.0);
    assert!(reference.len() >= 29);
    for &(tick, ref lattice) in &reference {
        assert_eq!(*lattice, expected(tick as f64 * TICK / 0.5), "tick {tick}");
    }
    for fps in [24.0, 30.0] {
        let run = run_at(fps, 0.5, false, 1.0);
        let shared = run.len().min(reference.len());
        assert!(shared >= 28);
        assert_eq!(run[..shared], reference[..shared], "{fps} fps at half speed");
    }
}

/// A machine that cannot keep up: late, jittery frames and a stall. Live
/// runs at most three ticks a frame, keeps one tick owed and drops the rest
/// of the owed time, so the liquid falls behind the transport; ticks are
/// never skipped or merged. Each tick still reads the field at the transport
/// time its own start maps to, never at the frame that runs it.
#[test]
fn liquid_forces_per_tick_survive_late_frames() {
    let mut rig = Rig::new();
    let mut transport = 0.0;
    let mut ran = 0u64;
    let mut drops = 0;
    let mut pinned = std::collections::HashMap::new();
    let intervals = [1.0, 2.6, 0.4, 1.9, 30.0, 1.0, 2.2, 0.7, 3.4, 1.0];
    rig.frame(0.0, TICK, 1.0, false, &modulated).unwrap();
    for step in 0..120 {
        let interval = TICK * intervals[step % intervals.len()];
        transport += interval;
        let starts: Vec<_> = (0..4)
            .map(|offset| pinned.get(&(ran + offset)).copied().or(rig.clock.tick_start(ran + offset)))
            .collect();
        let (frame, laid) = rig.frame(transport, interval, 1.0, false, &modulated).unwrap();
        assert_eq!(first_tick(&frame), ran, "frame {step}: no tick skipped or merged");
        if frame.dropped_seconds > 0.0 {
            // The owed tick now starts in the past; it reads this frame.
            drops += 1;
            let mut owed = rig.clock.ticks_done();
            while rig.clock.tick_start(owed).is_some_and(|start| start <= transport) {
                pinned.insert(owed, transport);
                owed += 1;
            }
        }
        for (offset, lattice) in rig.tick_lattices(&frame, &laid).into_iter().enumerate() {
            let start = starts[offset].expect("a running clock maps every tick");
            assert!(start < transport, "frame {step}: tick {} starts before the frame that runs it", ran + offset as u64);
            assert_eq!(lattice, expected(start), "frame {step}: tick {}", ran + offset as u64);
        }
        ran += u64::from(frame.ticks);
    }
    assert!(rig.clock.ticks_done() == ran && ran > 100);
    assert!(drops > 0, "the stalls dropped owed time");
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
    // The field was never sampled for this tick, so the frame fails.
    LiquidFields::default().prepare(lattice(), Some(&broken), &clock, &next, &impulses).unwrap_err();
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
    let mut rig = Rig::new();
    let held = |_: f64| field.clone();
    rig.frame(0.0, TICK, 1.0, false, &held).unwrap();
    let (_, frame) = rig.frame(TICK, TICK, 1.0, false, &held).unwrap();
    assert_eq!(frame.force_lattices, 1);
    assert_eq!(frame.impulse_tick, None);
    let fields = &rig.fields;
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
    rig.fields.forces_dirty = false;
    let (still, _) = rig.frame(2.0 * TICK, TICK, 1.0, false, &held).unwrap();
    assert!(!rig.fields.forces_dirty);
    let off = rig.fields.prepare(lattice, None, &rig.clock, &still, &rig.impulses).unwrap();
    assert_eq!(off.force_lattices, 0);
    assert!(rig.fields.forces().is_empty());
}

#[test]
fn liquid_force_lattice_refuses_a_non_finite_field() {
    let field = FieldValue::uniform([f32::MAX, 0.0, 0.0]).unwrap().scaled(f32::MAX).unwrap();
    let mut rig = Rig::new();
    let broken = |_: f64| field.clone();
    rig.frame(0.0, TICK, 1.0, false, &broken).unwrap();
    let error = rig.frame(TICK, TICK, 1.0, false, &broken).unwrap_err();
    assert!(error.contains("not finite"), "{error}");
}

/// A frame whose ticks the history replay never sampled is refused by name.
#[test]
fn liquid_forces_refuse_an_unsampled_tick() {
    let mut clock = LiquidClock::default();
    let mut impulses = LiquidImpulses::default();
    frame(&mut clock, &mut impulses, 0.0, TICK, 1.0);
    let next = frame(&mut clock, &mut impulses, 2.0 * TICK, TICK, 1.0);
    let field = FieldValue::uniform([1.0, 0.0, 0.0]).unwrap();
    let error = LiquidFields::default().prepare(lattice(), Some(&field), &clock, &next, &impulses).unwrap_err();
    assert!(error.contains("never sampled"), "{error}");
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
    let field_frame = fields.prepare(lattice, None, &clock, &next, &impulses).unwrap();
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
    assert_eq!(fields.prepare(lattice, None, &clock, &after, &impulses).unwrap().impulse_tick, None);
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
