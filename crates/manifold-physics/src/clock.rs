//! Transport-owned simulation intervals. Live covers the whole observed span;
//! export accepts complete project-frame intervals. Sequence is identity,
//! never elapsed time. Submission and fenced completion belong to consumers.

use std::sync::Arc;

use crate::Seconds;
use crate::stepping::{FramePlan, StepInterval};

pub const TICK: f64 = 1.0 / 60.0;

/// Accepted frame intervals and transport/display endpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockFrame {
    pub plan: FramePlan,
    pub first_sequence: u64,
    pub numerical_error: bool,
    pub offline: bool,
    project_interval: f64,
    transport_origin: f64,
    transport_first: u64,
    transport_intervals: u64,
    speed_history: Arc<Vec<SpeedAnchor>>,
    pub ticks: u32,
    pub epoch: u32,
    /// This frame starts a new simulation (first frame, reset, setup change or
    /// backward seek); the state reseeds before any tick runs.
    pub restarted: bool,
    /// Transport paused or Simulation Speed 0 now. Impulses fired now are
    /// discarded, so resume never bursts.
    pub held: bool,
    /// Simulated seconds at the end of this frame's ticks.
    pub simulation_time: f64,
    /// Simulation target for the observed transport; authored controls are
    /// sampled here. Live accepts this endpoint in full.
    pub target_time: f64,
    /// Display time `s = target − tick` (surface design D10).
    pub display_time: f64,
    /// Compatibility output for existing graphs: always zero.
    pub dropped_seconds: f64,
}

/// One transport clock shared by physics consumers.
#[derive(Clone, Debug, Default)]
pub struct SimulationClock {
    /// 0 before the first start, so outputs a domain holds while it waits
    /// (for a role's geometry, say) never share an epoch with the first
    /// simulation, which then seeds.
    epoch: u32,
    started: bool,
    last_transport: f64,
    previous_reset: Option<f32>,
    target_time: f64,
    ticks_done: u64,
    simulation_time: f64,
    offline: bool,
    project_interval: f64,
    transport_origin: f64,
    transport_done: u64,
    /// Speed over the interval after the last frame. Like every replayed
    /// control, a Speed edit takes effect from the frame that observes it.
    speed: f64,
    /// Retain the continuity anchor and all speed edits after accepted time.
    speed_history: Vec<SpeedAnchor>,
    history_snapshots: Vec<Arc<Vec<SpeedAnchor>>>,
}

/// Piecewise-constant speed, integrated once at each observed edit. Accepted
/// frames retain an immutable view; later edits cannot rewrite their endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpeedAnchor {
    transport: f64,
    simulation: f64,
    speed: f64,
}

fn map_transport(anchors: &[SpeedAnchor], transport: f64) -> f64 {
    let index = anchors.partition_point(|anchor| anchor.transport <= transport);
    anchors.get(index.saturating_sub(1)).map_or(0.0, |anchor| {
        anchor.simulation + (transport - anchor.transport).max(0.0) * anchor.speed
    })
}

impl SimulationClock {
    /// Observe controls without accepting a render interval or consuming ticks.
    /// History/source capture and rendering share this exact time mapping.
    pub fn observe_speed(&mut self, transport: f64, speed: f32) -> f64 {
        if !transport.is_finite() || !speed.is_finite() || speed < 0.0 {
            return self.target_time;
        }
        let observed_simulation = map_transport(&self.speed_history, transport);
        // A late source/audio observation may read accepted time, but may
        // only change the speed of time we have not accepted yet.
        let accepted_transport = if self.offline {
            self.transport_origin + self.transport_done as f64 * self.project_interval
        } else {
            self.last_transport
        };
        let transport = if self.started && !self.speed_history.is_empty() {
            transport.max(accepted_transport)
        } else {
            transport
        };
        let simulation = map_transport(&self.speed_history, transport);
        if self.speed_history.last().is_none_or(|last| {
            transport >= last.transport && last.speed != f64::from(speed)
        }) {
            let history = &mut self.speed_history;
            let anchor = SpeedAnchor { transport, simulation, speed: f64::from(speed) };
            if history.last().is_some_and(|last| last.transport == transport) {
                *history.last_mut().expect("last anchor") = anchor;
            } else {
                history.push(anchor);
            }
        }
        observed_simulation
    }

    pub fn accepted_time(&self) -> f64 {
        self.simulation_time
    }

    pub fn simulation_at(&self, transport: f64) -> f64 {
        map_transport(&self.speed_history, transport)
    }

    // Reuse retired immutable snapshots instead of allocating in the frame
    // path. Consumers keep their accepted view until they have drained it.
    fn snapshot_history(&mut self) -> Arc<Vec<SpeedAnchor>> {
        let index = self.history_snapshots.iter().position(|slot| Arc::strong_count(slot) == 1)
            .unwrap_or_else(|| {
                self.history_snapshots.push(Arc::new(Vec::with_capacity(256)));
                self.history_snapshots.len() - 1
            });
        let snapshot = Arc::get_mut(&mut self.history_snapshots[index]).expect("retired snapshot");
        snapshot.clear();
        snapshot.extend_from_slice(&self.speed_history);
        self.history_snapshots[index].clone()
    }

    /// The transport time tick `tick` starts at, under the current speed.
    /// None before the clock starts or while Speed is 0.
    pub fn tick_start(&self, tick: u64) -> Option<f64> {
        if !self.offline {
            return (self.started && tick == self.ticks_done).then_some(self.last_transport);
        }
        (self.started && self.speed > 0.0).then(|| {
            self.transport_origin
                + (self.transport_done + tick - self.ticks_done) as f64 * self.project_interval
        })
    }

    /// The transport of the last frame.
    pub fn transport(&self) -> f64 {
        self.last_transport
    }

    /// Ticks run since the epoch began: the next tick to run.
    pub fn ticks_done(&self) -> u64 {
        self.ticks_done
    }

    /// Every tick not yet run whose start lies in `(from, until]` of
    /// transport time, ascending, with its transport time. Live requests the
    /// next accepted boundary; export requests project-frame boundaries.
    pub fn tick_starts(&self, from: f64, until: f64, mut visit: impl FnMut(f64, u64)) {
        if !self.offline {
            if self.started && self.speed > 0.0 && until > self.last_transport && until > from {
                visit(until, self.ticks_done + 1);
            }
            return;
        }
        let mut tick = self.ticks_done;
        while let Some(transport) = self
            .tick_start(tick)
            .filter(|&transport| transport <= until)
        {
            if transport > from {
                visit(transport, tick);
            }
            tick += 1;
        }
    }

    /// Make the next frame a restart in a new epoch. The domain's
    /// `clear_state` calls this: the runtime's state reset (export start,
    /// resize, an idle chain) must reseed the liquid even when the transport
    /// runs on, and the epoch never repeats, so the state node reseeds too.
    pub fn restart(&mut self) {
        self.started = false;
    }

    /// Advance from transport. Live accepts one observed display interval.
    /// Offline accepts complete intervals of `project_interval` transport seconds;
    /// the export frame samples the last completed project frame.
    pub fn advance(
        &mut self,
        transport: f64,
        project_interval: f64,
        speed: f32,
        reset: f32,
        setup_changed: bool,
        offline: bool,
    ) -> ClockFrame {
        let mut numerical_error =
            !transport.is_finite() || !speed.is_finite() || speed < 0.0 || !reset.is_finite();
        // Invalid authored clock input has no meaningful elapsed duration. Keep
        // the previous endpoint and expose the fault; the next valid observation
        // still accounts for its full transport span.
        let transport = if transport.is_finite() {
            transport
        } else {
            self.last_transport
        };
        let speed = if speed.is_finite() && speed >= 0.0 {
            speed
        } else {
            self.speed as f32
        };
        let reset = if reset.is_finite() {
            reset
        } else {
            self.previous_reset.unwrap_or(0.0)
        };
        self.offline = offline;
        if offline {
            if project_interval.is_finite() && project_interval > 0.0 {
                self.project_interval = project_interval;
            } else {
                numerical_error = true;
                // No schedule can be inferred from an invalid project rate.
                return ClockFrame {
                    plan: FramePlan {
                        start: Seconds(self.simulation_time),
                        end: Seconds(self.simulation_time),
                        intervals: 0,
                    },
                    first_sequence: self.ticks_done,
                    numerical_error,
                    offline,
                    project_interval: 0.0,
                    transport_origin: self.transport_origin,
                    transport_first: self.transport_done,
                    transport_intervals: 0,
                    speed_history: self.snapshot_history(),
                    ticks: 0,
                    epoch: self.epoch,
                    restarted: false,
                    held: true,
                    simulation_time: self.simulation_time,
                    target_time: self.target_time,
                    display_time: self.simulation_time,
                    dropped_seconds: 0.0,
                };
            }
        }
        // A trigger publishes a counter; any change (undo included) resets once.
        let reset_edge = self
            .previous_reset
            .is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        let restarted =
            !self.started || setup_changed || reset_edge || transport < self.last_transport - 1e-9;
        let mut held = false;
        // Authored inputs were sampled at the last frame's target; within an
        // epoch the target never moves back past it. Live accepts the entire
        // span since the preceding endpoint.
        let speed = f64::from(speed);
        let transport_first = if restarted { 0 } else { self.transport_done };
        let transport_reached = if offline && !restarted {
            (((transport - self.transport_origin) / self.project_interval + 1e-9).floor() as u64)
                .max(self.transport_done)
        } else {
            transport_first
        };
        let accepted_transport = if offline && !restarted {
            self.transport_origin + transport_reached as f64 * self.project_interval
        } else {
            transport
        };
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.simulation_time = 0.0;
            self.speed = speed;
            self.transport_origin = transport;
            self.transport_done = 0;
            self.speed_history.clear();
            self.observe_speed(transport, speed as f32);
        } else {
            // The interval since the last frame ran at the last frame's speed.
            held = transport <= self.last_transport || speed <= 0.0;
            self.observe_speed(transport, speed as f32);
            let reached = self.simulation_at(accepted_transport);
            if reached.is_finite() {
                self.target_time = reached.max(self.target_time);
            } else {
                numerical_error = true;
            }
            if speed != self.speed {
                self.speed = speed;
            }
        }
        self.last_transport = transport;
        let first_sequence = self.ticks_done;
        let start = self.simulation_time;
        let ticks = if offline && self.speed_history.iter().all(|anchor| anchor.speed > 0.0) {
            transport_reached - transport_first
        } else if offline {
            (transport_first..transport_reached).filter(|&index| {
                let at = |i| self.simulation_at(self.transport_origin + i as f64 * self.project_interval);
                at(index + 1) > at(index)
            }).count() as u64
        } else {
            u64::from(self.target_time > start)
        };
        self.ticks_done += ticks;
        self.simulation_time = self.target_time;
        self.transport_done = transport_reached;
        let plan = FramePlan {
            start: Seconds(start),
            end: Seconds(self.simulation_time),
            intervals: ticks,
        };
        let speed_history = self.snapshot_history();
        let retained = self.speed_history.partition_point(|anchor| anchor.transport <= accepted_transport)
            .saturating_sub(1);
        self.speed_history.drain(..retained);
        ClockFrame {
            plan,
            first_sequence,
            numerical_error,
            offline,
            project_interval: self.project_interval,
            transport_origin: self.transport_origin,
            transport_first,
            transport_intervals: transport_reached - transport_first,
            speed_history,
            ticks: ticks as u32,
            epoch: self.epoch,
            restarted,
            held,
            simulation_time: self.simulation_time,
            target_time: self.target_time,
            display_time: (self.target_time - TICK).max(0.0),
            dropped_seconds: 0.0,
        }
    }
}

impl ClockFrame {
    /// Retain an unread export schedule without one allocation per observation.
    pub fn append(&mut self, next: &Self) -> bool {
        if !self.offline
            || !next.offline
            || self.epoch != next.epoch
            || self.project_interval != next.project_interval
            || self.transport_origin != next.transport_origin
            || self.speed_history != next.speed_history
            || self.first_sequence + u64::from(self.ticks) != next.first_sequence
            || self.transport_first + self.transport_intervals != next.transport_first
        {
            return false;
        }
        let Some(ticks) = self.ticks.checked_add(next.ticks) else {
            return false;
        };
        self.ticks = ticks;
        self.transport_intervals += next.transport_intervals;
        self.plan.end = next.plan.end;
        self.plan.intervals += next.plan.intervals;
        self.simulation_time = next.simulation_time;
        self.target_time = next.target_time;
        self.display_time = next.display_time;
        true
    }
    pub fn interval(&self, ordinal: u64) -> Option<StepInterval> {
        if self.offline {
            let endpoint = |index: u64| Seconds(map_transport(&self.speed_history,
                self.transport_origin + index as f64 * self.project_interval));
            if self.transport_intervals == u64::from(self.ticks) {
                return (ordinal < u64::from(self.ticks)).then(|| {
                    let index = self.transport_first + ordinal;
                    StepInterval::new(endpoint(index), endpoint(index + 1))
                });
            }
            (self.transport_first..self.transport_first + self.transport_intervals)
                .map(|index| StepInterval::new(endpoint(index), endpoint(index + 1)))
                .filter(|interval| interval.end.0 > interval.start.0)
                .nth(ordinal as usize)
        } else {
            self.plan.interval(ordinal)
        }
    }
    pub fn duration(&self) -> Seconds {
        if self.offline {
            self.interval(0).map_or(Seconds::ZERO, |interval| interval.duration())
        } else {
            Seconds(self.plan.end.0 - self.plan.start.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stepping::{CflPolicy, CflRestrictions, LiveStepSchedule};

    #[test]
    fn source_speed_observations_cannot_rewrite_accepted_intervals() {
        let mut clock = SimulationClock::default();
        clock.advance(2.0, TICK, 1.0, 0.0, false, true);
        assert!((clock.observe_speed(2.05, 2.0) - 0.05).abs() < 1e-12);
        assert!((clock.observe_speed(2.10, 2.0) - 0.15).abs() < 1e-12);
        let accepted = clock.advance(2.10, TICK, 2.0, 0.0, false, true);
        let intervals: Vec<_> = (0..u64::from(accepted.ticks))
            .map(|i| accepted.interval(i).unwrap()).collect();
        assert!((accepted.simulation_time - 0.15).abs() < 1e-12);
        assert!((intervals[2].end.0 - 0.05).abs() < 1e-12);
        assert!((intervals[3].duration().0 - 2.0 * TICK).abs() < 1e-12);
        clock.observe_speed(2.10, 0.5);
        clock.advance(2.2, TICK, 0.5, 0.0, false, true);
        assert_eq!(intervals, (0..u64::from(accepted.ticks))
            .map(|i| accepted.interval(i).unwrap()).collect::<Vec<_>>());
    }

    fn late_speed_observation_preserves_accepted_time(offline: bool) {
        let mut clock = SimulationClock::default();
        clock.advance(2.0, TICK, 1.0, 0.0, false, offline);
        // Offline stops at 2.1 even though the render observed 2.107.
        let previous = clock.advance(2.107, TICK, 1.0, 0.0, false, offline);
        let end = if offline { 2.0 + 6.0 * TICK } else { 2.107 };
        let samples: Vec<_> = (0..=100).map(|i| {
            let x = 2.0 + (end - 2.0) * f64::from(i) / 100.0;
            (x, clock.simulation_at(x))
        }).collect();
        let observed = clock.simulation_at(2.05);
        assert_eq!(clock.observe_speed(2.05, 3.0), observed);
        for (x, expected) in samples {
            assert_eq!(clock.simulation_at(x), expected, "past transport {x}");
        }
        assert_eq!(clock.simulation_at(end), previous.simulation_time);
        let future = end + TICK;
        assert_eq!(clock.simulation_at(future), previous.simulation_time + (future - end) * 3.0);
        let next = clock.advance(2.2, TICK, 3.0, 0.0, false, offline);
        assert_eq!(next.interval(0).unwrap().start.0, previous.simulation_time);
    }

    #[test]
    fn late_live_speed_observation_preserves_accepted_time() {
        late_speed_observation_preserves_accepted_time(false);
    }

    #[test]
    fn late_offline_speed_observation_preserves_accepted_time() {
        late_speed_observation_preserves_accepted_time(true);
    }

    #[test]
    fn historical_speed_schedule_is_independent_of_export_partition() {
        let run = |fps: u32| {
            let mut clock = SimulationClock::default();
            clock.advance(0.0, TICK, 1.0, 0.0, false, true);
            let mut intervals = Vec::new();
            for sample in 1..=240 {
                let transport = sample as f64 / 240.0;
                let speed = if sample < 53 { 1.0 } else if sample < 149 { 2.0 } else { 0.5 };
                clock.observe_speed(transport, speed);
                if sample % (240 / fps) == 0 {
                    let frame = clock.advance(transport, TICK, speed, 0.0, false, true);
                    intervals.extend((0..u64::from(frame.ticks)).map(|i| frame.interval(i).unwrap()));
                }
            }
            intervals
        };
        let expected = run(60);
        assert_eq!(expected.len(), 60);
        for fps in [1, 20, 24, 30, 120] {
            assert_eq!(run(fps), expected, "export {fps} fps");
        }
    }

    #[test]
    fn live_clock_covers_every_frame_at_20_24_30_60_fps() {
        for fps in [20, 24, 30, 60] {
            let mut clock = SimulationClock::default();
            let mut completed = 0.0;
            for index in 0..=fps {
                let time = f64::from(index) / f64::from(fps);
                let frame = clock.advance(time, 1.0 / f64::from(fps), 1.0, 0.0, false, false);
                assert_eq!(frame.plan.start.0, completed);
                if let Some(interval) = frame.interval(0) {
                    let mut schedule =
                        LiveStepSchedule::new(interval.start, interval.duration(), 1, 6).value;
                    let mut cap = false;
                    while let Some(step) = schedule.next(Seconds(1e-6)).value {
                        assert_eq!(step.interval.start.0, completed);
                        completed = step.interval.end.0;
                        cap |= step.hit_cap;
                    }
                    assert!(cap, "tiny CFL must hit the cap");
                    assert_eq!(schedule.steps_taken(), 6);
                }
                assert_eq!(completed, time);
                assert_eq!(frame.dropped_seconds, 0.0);
            }
            assert_eq!(completed, 1.0);
        }
    }

    #[test]
    fn export_matches_live_project_intervals_and_cfl_steps() {
        for project_fps in [24, 30, 60, 120] {
            let project_interval = 1.0 / f64::from(project_fps);
            for speed in [0.5, 1.0, 2.0] {
                let run = |fps, offline| {
                    let mut clock = SimulationClock::default();
                    let mut intervals = Vec::new();
                    let mut steps = Vec::new();
                    for index in 0..=fps {
                        let frame = clock.advance(
                            f64::from(index) * (1.0 / f64::from(fps)),
                            project_interval,
                            speed,
                            0.0,
                            false,
                            offline,
                        );
                        for ordinal in 0..u64::from(frame.ticks) {
                            let interval = frame.interval(ordinal).unwrap();
                            intervals.push(interval);
                            let mut schedule =
                                LiveStepSchedule::new(interval.start, interval.duration(), 2, 6)
                                    .value;
                            while let Some(step) = schedule
                                .next(
                                    CflPolicy::default()
                                        .duration(
                                            interval.duration(),
                                            if steps.len() % 2 == 0 { 1e7 } else { 1e3 },
                                            CflRestrictions::default(),
                                        )
                                        .value,
                                )
                                .value
                            {
                                steps.push(step);
                            }
                            assert!(schedule.steps_taken() <= 6);
                        }
                    }
                    (intervals, steps)
                };
                let live = run(project_fps, false);
                for export_fps in [20, 24, 30, 60] {
                    let export = run(export_fps, true);
                    assert_eq!(
                        live, export,
                        "project {project_fps}, export {export_fps}, speed {speed}"
                    );
                }
            }
        }
    }

    #[test]
    fn live_clock_invalid_observation_reports_and_retains_transport_span() {
        let mut clock = SimulationClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        let invalid = clock.advance(f64::NAN, TICK, 1.0, 0.0, false, false);
        assert!(invalid.numerical_error);
        assert_eq!(invalid.ticks, 0);
        let resumed = clock.advance(0.5, TICK, 1.0, 0.0, false, false);
        assert_eq!(resumed.plan.start, Seconds::ZERO);
        assert_eq!(resumed.plan.end, Seconds(0.5));
    }
}
