//! CPU-only specification for BUG-7qzk; compiled only by the crate's tests.
//! Not connected to LiquidClock, the renderer, or a GPU solver.
//! CFL step selection is ported from FLIP Fluids' fluidsimulation.cpp
//! (_calculateNextTimeStep), Ryan L. Guy & Dennis Fassbaender, MIT.
//! See THIRD_PARTY_NOTICES.md and docs/LIVE_SIM_CLOCK_DESIGN.md.

use manifold_physics::Seconds;

const TICK: f64 = 1.0 / 60.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Step {
    start: Seconds,
    end: Seconds,
}

impl Step {
    fn duration(self) -> Seconds {
        Seconds(self.end.0 - self.start.0)
    }

    /// Events belong to [start, end). Equal-time events preserve input order.
    /// The caller passes sorted, epoch-local timestamps and retains end events
    /// for the following interval. Integrate to the hit, apply it, then continue.
    fn visit(self, hits: &[Seconds], mut visit: impl FnMut(Action)) {
        assert!(hits.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert!(hits.iter().all(|hit| hit.0.is_finite()));
        let mut at = self.start;
        for (index, &hit) in hits.iter().enumerate() {
            if hit.0 < self.start.0 || hit.0 >= self.end.0 {
                continue;
            }
            if hit.0 > at.0 {
                visit(Action::Integrate(Step {
                    start: at,
                    end: hit,
                }));
            }
            visit(Action::Hit(index));
            at = hit;
        }
        if at.0 < self.end.0 {
            visit(Action::Integrate(Step {
                start: at,
                end: self.end,
            }));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Integrate(Step),
    Hit(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    epoch: u32,
    restarted: bool,
    held: bool,
    target: Seconds,
    simulation: Seconds,
    display: Seconds,
    start: Seconds,
    steps: u64,
    /// Time covered above one nominal tick per budgeted interval, this frame.
    /// This is not lag: successfully stretched time has already been simulated.
    stretched: Seconds,
}

impl Frame {
    /// Allocation-free equal partition of the complete owed fixed-tick span.
    fn step(self, index: u64) -> Step {
        assert!(index < self.steps);
        let span = self.simulation.0 - self.start.0;
        Step {
            start: Seconds(self.start.0 + span * index as f64 / self.steps as f64),
            end: Seconds(self.start.0 + span * (index + 1) as f64 / self.steps as f64),
        }
    }

    fn lag(self, completed: Seconds) -> Seconds {
        Seconds((self.target.0 - completed.0).max(0.0))
    }
}

#[derive(Default)]
struct Clock {
    epoch: u32,
    started: bool,
    last_transport: f64,
    previous_reset: Option<f32>,
    anchor_transport: f64,
    anchor_target: f64,
    speed: f64,
    target: f64,
    ticks_covered: u64,
}

impl Clock {
    fn restart(&mut self) {
        self.started = false;
    }

    /// Mirrors LiquidClock's epoch and speed-anchor semantics. CPU execution
    /// is synchronous here: planned intervals are considered completed. The
    /// production seam must acknowledge completion separately for HUD lag.
    fn advance(&mut self, input: Input) -> Frame {
        let Input {
            transport,
            speed,
            reset,
            setup_changed,
            offline,
            budget,
        } = input;
        assert!(transport.0.is_finite() && speed.is_finite() && speed >= 0.0);
        let restarted = !self.started
            || setup_changed
            || self
                .previous_reset
                .is_some_and(|previous| previous != reset)
            || transport.0 < self.last_transport - 1e-9;
        self.previous_reset = Some(reset);
        let held = !restarted && (transport.0 <= self.last_transport || speed == 0.0);
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target = 0.0;
            self.ticks_covered = 0;
            self.anchor_target = 0.0;
            self.anchor_transport = transport.0;
            self.speed = speed;
        } else {
            self.target = self.target.max(
                self.anchor_target + (transport.0 - self.anchor_transport).max(0.0) * self.speed,
            );
            if speed != self.speed {
                self.speed = speed;
                self.anchor_target = self.target;
                self.anchor_transport = transport.0;
            }
        }
        self.last_transport = transport.0;
        let start = self.ticks_covered as f64 * TICK;
        let reached = (self.target / TICK + 1e-9).floor() as u64;
        let due = reached.saturating_sub(self.ticks_covered);
        // Zero allowance means blocked, not time discarded. Offline ignores
        // the live budget entirely, including zero.
        let steps = if offline {
            due
        } else {
            due.min(u64::from(budget))
        };
        if steps > 0 {
            self.ticks_covered = reached;
        }
        Frame {
            epoch: self.epoch,
            restarted,
            held,
            target: Seconds(self.target),
            simulation: Seconds(self.ticks_covered as f64 * TICK),
            display: Seconds((self.target - TICK).max(0.0)),
            start: Seconds(start),
            steps,
            stretched: Seconds(if steps > 0 {
                (due - steps) as f64 * TICK
            } else {
                0.0
            }),
        }
    }
}

#[derive(Clone, Copy)]
struct Input {
    transport: Seconds,
    speed: f64,
    reset: f32,
    setup_changed: bool,
    offline: bool,
    budget: u32,
}

/// Port of fluidsimulation.cpp:11163–11194, with the caller supplying the
/// measured/predicted maximum of marker and relevant obstacle speeds.
/// Optional enabled restrictions use the reference's constants unchanged.
fn cfl_step(
    frame: Seconds,
    cell_size: f64,
    cfl: f64,
    max_speed: f64,
    surface_tension: Option<(f64, f64)>,
    color_mixing_rate: Option<f64>,
) -> Seconds {
    assert!(frame.0.is_finite() && frame.0 > 0.0);
    assert!(cell_size.is_finite() && cell_size > 0.0 && cfl.is_finite() && cfl >= 1.0);
    assert!(max_speed.is_finite() && max_speed >= 0.0);
    let eps = 1e-6;
    let mut limit = cfl * cell_size / (max_speed + eps);
    if let Some((condition, constant)) = surface_tension {
        assert!(
            condition.is_finite() && condition > 0.0 && constant.is_finite() && constant >= 0.0
        );
        limit = limit.min(condition * (cell_size.powi(3)).sqrt() * (1.0 / (constant + eps)).sqrt());
    }
    if let Some(rate) = color_mixing_rate {
        assert!(rate.is_finite() && rate >= 0.0);
        limit = limit.min(1.0 / (rate + eps));
    }
    let duration = frame.0 / (frame.0 / limit).ceil().max(1.0);
    assert!(duration.is_finite() && duration > 0.0);
    Seconds(duration)
}

fn input(time: f64, budget: u32) -> Input {
    Input {
        transport: Seconds(time),
        speed: 1.0,
        reset: 0.0,
        setup_changed: false,
        offline: false,
        budget,
    }
}

#[test]
fn live_sim_clock_same_time_at_20_24_30_60_fps() {
    for budget in [1, 2, 3] {
        for fps in [20, 24, 30, 60] {
            let mut clock = Clock::default();
            clock.advance(input(0.0, budget));
            let mut integrated = 0.0;
            for frame in 1..=fps * 10 {
                let out = clock.advance(input(f64::from(frame) / f64::from(fps), budget));
                assert!(out.steps <= u64::from(budget));
                for i in 0..out.steps {
                    integrated += out.step(i).duration().0;
                }
                assert!(out.lag(out.simulation).0 < TICK + 1e-9);
            }
            assert!(
                (integrated - 10.0).abs() < 1e-10,
                "{fps} fps, budget {budget}"
            );
        }
    }
}

#[test]
fn live_sim_clock_hits_land_inside_long_steps() {
    let mut clock = Clock::default();
    clock.advance(input(0.0, 1));
    let out = clock.advance(input(6.0 * TICK, 1));
    // Audio and force hits on distinct inner ticks, equal-time hits, and a
    // non-grid timestamp. Motion must reflect each impulse's actual moment.
    let hits = [
        Seconds(0.0),
        Seconds(TICK),
        Seconds(2.0 * TICK),
        Seconds(2.0 * TICK),
        Seconds(2.5 * TICK),
        Seconds(6.0 * TICK),
    ];
    let (mut x, mut velocity, mut now) = (0.0, 0.0, 0.0);
    let mut applied = Vec::new();
    out.step(0).visit(&hits, |action| match action {
        Action::Integrate(step) => {
            x += velocity * step.duration().0;
            now = step.end.0;
        }
        Action::Hit(i) => {
            assert_eq!(now, hits[i].0);
            velocity += 1.0;
            applied.push(i);
        }
    });
    assert_eq!(applied, [0, 1, 2, 3, 4]);
    let expected: f64 = hits[..5].iter().map(|hit| out.simulation.0 - hit.0).sum();
    assert!((x - expected).abs() < 1e-12);
    let next = clock.advance(input(7.0 * TICK, 1));
    next.step(0).visit(&hits, |action| {
        if let Action::Hit(i) = action {
            applied.push(i);
        }
    });
    assert_eq!(applied, [0, 1, 2, 3, 4, 5]);
}

#[test]
fn live_sim_clock_export_unchanged() {
    use crate::node_graph::liquid::clock::LiquidClock;
    let mut current = LiquidClock::default();
    let mut reference = Clock::default();
    for time in [0.0, TICK, 0.041, 0.5, 1.0, 2.0] {
        let out = reference.advance(Input {
            offline: true,
            ..input(time, 0)
        });
        let old = current.advance(time, TICK, 1.0, 0.0, false, true);
        assert_eq!(out.steps, u64::from(old.ticks));
        assert_eq!(out.simulation.0, old.simulation_time);
        assert_eq!(out.target.0, old.target_time);
        assert_eq!(out.stretched.0, 0.0);
        for i in 0..out.steps {
            assert!((out.step(i).duration().0 - TICK).abs() < 1e-12);
        }
    }
}

#[test]
fn live_sim_clock_pause_seek_speed_reset_match_today() {
    use crate::node_graph::liquid::clock::LiquidClock;
    let mut current = LiquidClock::default();
    let mut reference = Clock::default();
    let controls = [
        (0, 1.0, 0.0, false),
        (1, 1.0, 0.0, false),
        (1, 1.0, 0.0, false),
        (2, 0.0, 0.0, false),
        (3, 0.5, 0.0, false),
        (4, 0.5, 0.0, false),
        (5, 0.5, 0.0, false),
        (6, 0.5, 0.0, false),
        (7, 0.5, 0.0, false),
        (8, 1.0, 1.0, false),
        (2, 1.0, 1.0, false),
        (3, 1.0, 1.0, false),
        (4, 1.0, 1.0, true),
    ];
    for (tick, speed, reset, setup_changed) in controls {
        let time = f64::from(tick) * TICK;
        let out = reference.advance(Input {
            speed,
            reset,
            setup_changed,
            ..input(time, 3)
        });
        let old = current.advance(time, TICK, speed as f32, reset, setup_changed, false);
        assert_eq!(
            (out.epoch, out.restarted, out.held),
            (old.epoch, old.restarted, old.held)
        );
        assert_eq!(
            (out.simulation.0, out.target.0, out.display.0),
            (old.simulation_time, old.target_time, old.display_time)
        );
    }
    reference.restart();
    current.restart();
    let out = reference.advance(input(5.0 * TICK, 3));
    let old = current.advance(5.0 * TICK, TICK, 1.0, 0.0, false, false);
    assert_eq!(
        (out.epoch, out.restarted, out.simulation.0),
        (old.epoch, old.restarted, old.simulation_time)
    );
}

#[test]
fn live_sim_clock_pause_resume_same_epoch() {
    let mut paused = Clock::default();
    let mut straight = Clock::default();
    for i in 0..=10 {
        paused.advance(input(i as f64 * TICK, 1));
        straight.advance(input(i as f64 * TICK, 1));
    }
    let held = paused.advance(input(10.0 * TICK, 1));
    for _ in 0..100 {
        assert_eq!(paused.advance(input(10.0 * TICK, 1)), held);
    }
    for i in 11..=20 {
        assert_eq!(
            paused.advance(input(i as f64 * TICK, 1)),
            straight.advance(input(i as f64 * TICK, 1))
        );
    }
}

#[test]
fn live_sim_clock_blocked_retains_debt_then_stretches() {
    let mut clock = Clock::default();
    clock.advance(input(0.0, 1));
    let blocked = clock.advance(input(1.0, 0));
    assert_eq!(
        (blocked.steps, blocked.target.0, blocked.simulation.0),
        (0, 1.0, 0.0)
    );
    assert_eq!(blocked.lag(Seconds(0.0)).0, 1.0);
    let caught = clock.advance(input(2.0, 3));
    assert_eq!((caught.steps, caught.simulation.0), (3, 2.0));
    assert!((caught.stretched.0 - (2.0 - 3.0 * TICK)).abs() < 1e-12);
    assert_eq!(caught.lag(caught.simulation).0, 0.0);
}

#[test]
fn live_sim_clock_cfl_reference_rule() {
    // eps makes the speed restriction slightly smaller than 0.05, so
    // ceil(0.1 / limit) is THREE, not two. This catches dropping epsilon.
    let step = cfl_step(Seconds(0.1), 0.1, 2.0, 4.0, None, None);
    assert_eq!(step.0, 0.1 / 3.0);
    assert_eq!(cfl_step(Seconds(0.1), 0.1, 2.0, 0.0, None, None).0, 0.1);
    let tension = cfl_step(Seconds(0.1), 0.1, 2.0, 0.0, Some((1.0, 1.0)), None);
    assert_eq!(tension.0, 0.025);
    let color = cfl_step(Seconds(0.1), 0.1, 2.0, 0.0, None, Some(40.0));
    assert_eq!(color.0, 0.02);
}
