//! Transport-owned simulation intervals. Live covers the whole observed span;
//! export retains the original fixed 60 Hz tick sequence. Sequence is identity,
//! never elapsed time. Submission and fenced completion belong to consumers.

use crate::Seconds;
use crate::stepping::{FramePlan, StepInterval};

pub const TICK: f64 = 1.0 / 60.0;

/// Accepted frame intervals and transport/display endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockFrame {
    pub plan: FramePlan,
    pub first_sequence: u64,
    pub numerical_error: bool,
    pub offline: bool,
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
    /// Speed over the interval after the last frame. Like every replayed
    /// control, a Speed edit takes effect from the frame that observes it.
    speed: f64,
    /// Transport and target where the current speed began (a start, a speed
    /// edit). Targets and tick starts are measured from here, so
    /// they never accumulate rounding and agree at every frame rate.
    anchor_transport: f64,
    anchor_target: f64,
}

impl SimulationClock {
    /// The transport time tick `tick` starts at, under the current speed.
    /// None before the clock starts or while Speed is 0.
    pub fn tick_start(&self, tick: u64) -> Option<f64> {
        if !self.offline {
            return (self.started && tick == self.ticks_done).then_some(self.last_transport);
        }
        (self.started && self.speed > 0.0).then(|| {
            self.anchor_transport + ((tick as f64) * TICK - self.anchor_target) / self.speed
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
    /// next accepted boundary; export retains nominal tick starts.
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

    /// Advance by one display frame from transport. The retained host-delta
    /// argument preserves the caller seam; it never limits accepted time.
    pub fn advance(
        &mut self,
        transport: f64,
        _frame_interval: f64,
        speed: f32,
        reset: f32,
        setup_changed: bool,
        offline: bool,
    ) -> ClockFrame {
        let mut numerical_error = !transport.is_finite() || !speed.is_finite() || speed < 0.0 || !reset.is_finite();
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
        let reset = if reset.is_finite() { reset } else { self.previous_reset.unwrap_or(0.0) };
        self.offline = offline;
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
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.simulation_time = 0.0;
            self.speed = speed;
            self.anchor_transport = transport;
            self.anchor_target = 0.0;
        } else {
            // The interval since the last frame ran at the last frame's speed.
            held = transport <= self.last_transport || speed <= 0.0;
            let reached =
                self.anchor_target + (transport - self.anchor_transport).max(0.0) * self.speed;
            if reached.is_finite() {
                self.target_time = reached.max(self.target_time);
            } else {
                numerical_error = true;
            }
            if speed != self.speed {
                self.speed = speed;
                self.anchor_transport = transport;
                self.anchor_target = self.target_time;
            }
        }
        self.last_transport = transport;
        let first_sequence = self.ticks_done;
        let start = self.simulation_time;
        let ticks = if offline {
            ((self.target_time / TICK + 1e-9).floor() as u64).saturating_sub(self.ticks_done)
        } else {
            u64::from(self.target_time > start)
        };
        self.ticks_done += ticks;
        self.simulation_time = if offline {
            self.ticks_done as f64 * TICK
        } else {
            self.target_time
        };
        let plan = FramePlan {
            start: Seconds(start),
            end: Seconds(self.simulation_time),
            intervals: ticks,
        };
        ClockFrame {
            plan,
            first_sequence,
            numerical_error,
            offline,
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
    pub fn interval(self, ordinal: u64) -> Option<StepInterval> {
        if self.offline {
            (ordinal < u64::from(self.ticks)).then(|| {
                let tick = self.first_sequence + ordinal;
                StepInterval::new(Seconds(tick as f64 * TICK), Seconds((tick + 1) as f64 * TICK))
            })
        } else {
            self.plan.interval(ordinal)
        }
    }
    pub fn duration(self) -> Seconds {
        if self.offline {
            Seconds(TICK)
        } else {
            Seconds(self.plan.end.0 - self.plan.start.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stepping::LiveStepSchedule;

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
    fn live_clock_export_keeps_original_tick_bits_and_ordinals() {
        for fps in [20, 24, 30, 60] {
            let mut clock = SimulationClock::default();
            let mut completed_ticks = 0;
            for index in 0..=fps {
                let time = f64::from(index) / f64::from(fps);
                let expected_ticks = (time / TICK + 1e-9).floor() as u64;
                let frame = clock.advance(time, 1.0 / f64::from(fps), 1.0, 0.0, false, true);
                assert_eq!(frame.first_sequence, completed_ticks);
                assert_eq!(u64::from(frame.ticks), expected_ticks - completed_ticks);
                assert_eq!(frame.duration().0.to_bits(), TICK.to_bits());
                assert_eq!(
                    frame.simulation_time.to_bits(),
                    (expected_ticks as f64 * TICK).to_bits()
                );
                completed_ticks = expected_ticks;
            }
            assert_eq!(completed_ticks, 60);
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
