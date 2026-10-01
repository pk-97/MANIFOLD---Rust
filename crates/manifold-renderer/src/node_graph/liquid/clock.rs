//! The fixed 60 Hz clock a GPU liquid domain owns
//! (`docs/LIQUID_SOLVER_SEAM_DESIGN.md` section 3.4; GPU_MPM_SOLVER_DESIGN.md
//! D8). Box3D and FLIP keep `HeldClock` (D9).

use crate::node_graph::fluid::TICK;

/// One frame of the clock: how many fixed ticks to run and where the display
/// sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockFrame {
    pub ticks: u32,
    pub epoch: u32,
    /// This frame starts a new simulation (first frame, reset, setup change or
    /// backward seek); the state reseeds before any tick runs.
    pub restarted: bool,
    /// Transport paused or Simulation Speed 0: simulated time did not move.
    /// Impulses fired now are discarded, so resume never bursts.
    pub held: bool,
    /// Simulated seconds at the end of this frame's ticks.
    pub simulation_time: f64,
    /// Simulated seconds this display frame reached (at most one tick past
    /// `simulation_time` live); authored controls are sampled here.
    pub target_time: f64,
    /// Display time `s = target − tick` (surface design D10).
    pub display_time: f64,
    /// Simulated time dropped under live overload since the epoch began.
    pub dropped_seconds: f64,
}

/// Most live ticks one display frame may run: 1 at 60 fps, 2 at 30, 3 at 24.
/// A slow frame never earns more than this, so live cannot spiral.
pub const MAX_LIVE_TICKS: u32 = 3;

/// Transport and Speed build a target time; live runs at most the frame's
/// share of ticks and drops the rest (reported), keeping at most one tick of
/// jitter debt; offline runs every due tick.
#[derive(Clone, Debug, Default)]
pub struct LiquidClock {
    /// 0 before the first start, so outputs a domain holds while it waits
    /// (for a role's geometry, say) never share an epoch with the first
    /// simulation, which then seeds.
    epoch: u32,
    started: bool,
    last_transport: f64,
    previous_reset: Option<f32>,
    target_time: f64,
    ticks_done: u64,
    dropped_seconds: f64,
    tick_cap: Option<u32>,
}

impl LiquidClock {
    /// Cap the ticks of the frames that follow, live and offline alike: at most
    /// `cap` run, one tick of debt is kept and the rest is dropped, reported.
    /// A coupled domain caps at 0 while its body reaction is pending and at 1
    /// otherwise (section 3.3). `None` restores the uncapped clock.
    pub fn set_tick_cap(&mut self, cap: Option<u32>) {
        self.tick_cap = cap;
    }

    /// Make the next frame a restart in a new epoch. The domain's
    /// `clear_state` calls this: the runtime's state reset (export start,
    /// resize, an idle chain) must reseed the liquid even when the transport
    /// runs on, and the epoch never repeats, so the state node reseeds too.
    pub fn restart(&mut self) {
        self.started = false;
    }

    /// Advance by one display frame. `frame_interval` is this frame's host
    /// delta in seconds; it only sets the live tick allowance.
    pub fn advance(
        &mut self,
        transport: f64,
        frame_interval: f64,
        speed: f32,
        reset: f32,
        setup_changed: bool,
        offline: bool,
    ) -> ClockFrame {
        // A trigger publishes a counter; any change (undo included) resets once.
        let reset_edge = self.previous_reset.is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        let restarted = !self.started
            || setup_changed
            || reset_edge
            || transport < self.last_transport - 1e-9;
        let mut held = false;
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.dropped_seconds = 0.0;
        } else {
            let advance = (transport - self.last_transport).max(0.0) * f64::from(speed);
            held = advance <= 0.0;
            self.target_time += advance;
        }
        self.last_transport = transport;
        let due = ((self.target_time / TICK + 1e-9).floor() as u64).saturating_sub(self.ticks_done);
        let ticks = if offline && self.tick_cap.is_none() {
            due
        } else {
            let allowance = match self.tick_cap {
                Some(cap) => u64::from(cap),
                None => ((frame_interval / TICK) - 1e-6).ceil().clamp(1.0, f64::from(MAX_LIVE_TICKS)) as u64,
            };
            let run = due.min(allowance);
            // Keep one tick of scheduling jitter; drop the rest visibly.
            let dropped = due.saturating_sub(run).saturating_sub(1);
            if dropped > 0 {
                let seconds = dropped as f64 * TICK;
                self.target_time -= seconds;
                self.dropped_seconds += seconds;
            }
            run
        };
        self.ticks_done += ticks;
        ClockFrame {
            ticks: ticks as u32,
            epoch: self.epoch,
            restarted,
            held,
            simulation_time: self.ticks_done as f64 * TICK,
            target_time: self.target_time,
            display_time: (self.target_time - TICK).max(0.0),
            dropped_seconds: self.dropped_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(clock: &mut LiquidClock, frames: &[(f64, f64)], offline: bool) -> Vec<ClockFrame> {
        frames
            .iter()
            .map(|&(t, dt)| clock.advance(t, dt, 1.0, 0.0, false, offline))
            .collect()
    }

    #[test]
    fn liquid_clock_live_caps_ticks_per_frame() {
        let mut clock = LiquidClock::default();
        // 60 fps: one tick per frame after the first.
        let frames: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * TICK, TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[0].restarted);
        assert!(out[1..].iter().all(|f| f.ticks == 1 && !f.restarted));
        // A one-second stall: the frame runs its allowance, keeps one tick of
        // debt and drops the rest, reported.
        let stalled = clock.advance(10.0 * TICK + 1.0, 1.0, 1.0, 0.0, false, false);
        assert_eq!(stalled.ticks, MAX_LIVE_TICKS);
        let owed = 60u32;
        let dropped_ticks = owed - MAX_LIVE_TICKS - 1;
        assert!((stalled.dropped_seconds - f64::from(dropped_ticks) * TICK).abs() < 1e-9);
        // 30 fps runs two ticks per frame.
        let mut clock = LiquidClock::default();
        let frames: Vec<(f64, f64)> = (0..=6).map(|i| (i as f64 * 2.0 * TICK, 2.0 * TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[1..].iter().all(|f| f.ticks == 2));
    }

    #[test]
    fn liquid_clock_export_runs_every_tick() {
        let mut clock = LiquidClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, true);
        let frame = clock.advance(1.0, 1.0, 1.0, 0.0, false, true);
        assert_eq!(frame.ticks, 60);
        assert_eq!(frame.dropped_seconds, 0.0);
    }

    #[test]
    fn liquid_clock_pause_reset_speed_and_seek() {
        let mut clock = LiquidClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        let a = clock.advance(TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!(a.ticks, 1);
        assert!(!a.held);
        // Paused transport holds.
        let held = clock.advance(TICK, 0.0, 1.0, 0.0, false, false);
        assert_eq!(held.ticks, 0);
        assert!(held.held);
        assert_eq!(held.simulation_time, a.simulation_time);
        // Speed 0 holds while transport runs.
        assert!(clock.advance(2.0 * TICK, TICK, 0.0, 0.0, false, false).held);
        // Speed 0.5 runs a tick every other frame and never holds.
        let frames: Vec<_> = (3..7)
            .map(|i| clock.advance(i as f64 * TICK, TICK, 0.5, 0.0, false, false))
            .collect();
        assert_eq!(frames.iter().map(|f| f.ticks).sum::<u32>(), 2);
        assert!(frames.iter().all(|f| !f.held));
        // A changed reset counter restarts in a new epoch.
        let reset = clock.advance(7.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!(reset.restarted);
        assert!(!reset.held);
        assert_eq!(reset.epoch, 2);
        assert_eq!(reset.ticks, 0);
        // Seeking backwards restarts too.
        let seek = clock.advance(2.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!(seek.restarted);
        assert_eq!(seek.epoch, 3);
        // Display sits one tick behind the target.
        let next = clock.advance(3.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!((next.display_time - 0.0).abs() < 1e-12);
        assert_eq!(next.ticks, 1);
    }

    /// Pause runs zero ticks however long the host keeps drawing, and play
    /// carries on in the same epoch with exactly the ticks an uninterrupted
    /// run has at the same transport time: the water is the same water.
    #[test]
    fn liquid_clock_pause_resume_continues_the_same_state() {
        let mut paused = LiquidClock::default();
        let mut straight = LiquidClock::default();
        for i in 0..=10 {
            let t = i as f64 * TICK;
            paused.advance(t, TICK, 1.0, 0.0, false, false);
            straight.advance(t, TICK, 1.0, 0.0, false, false);
        }
        let before = paused.advance(10.0 * TICK, TICK, 1.0, 0.0, false, false);
        // The host keeps drawing with real frame deltas while the transport
        // stands still, including a long stall.
        for interval in [TICK, TICK, 0.5, TICK, 0.0, 2.0] {
            let held = paused.advance(10.0 * TICK, interval, 1.0, 0.0, false, false);
            assert_eq!(held.ticks, 0);
            assert_eq!(held, before);
        }
        for i in 11..=20 {
            let t = i as f64 * TICK;
            let resumed = paused.advance(t, TICK, 1.0, 0.0, false, false);
            let reference = straight.advance(t, TICK, 1.0, 0.0, false, false);
            assert!(!resumed.restarted);
            assert_eq!(resumed, reference, "frame {i} after resume");
        }
    }

    /// Live frames with jittery host deltas reach the same simulated time as
    /// an offline run at the same transport times while nothing drops: export
    /// and realtime show the same water at the same instant.
    #[test]
    fn liquid_clock_live_matches_offline_at_the_same_transport_time() {
        let mut live = LiquidClock::default();
        let mut offline = LiquidClock::default();
        let mut transport = 0.0;
        // A display near 60 fps that never runs more than three ticks late.
        let jitter = [1.0, 0.7, 1.4, 1.0, 0.9, 1.6, 0.5, 1.0, 1.2, 0.8];
        for step in 0..200 {
            let interval = TICK * jitter[step % jitter.len()];
            transport += interval;
            let a = live.advance(transport, interval, 1.0, 0.0, false, false);
            let b = offline.advance(transport, interval, 1.0, 0.0, false, true);
            assert_eq!(a.dropped_seconds, 0.0, "frame {step} dropped live");
            // Live keeps at most one tick of jitter debt; it never runs ahead.
            let behind = b.simulation_time - a.simulation_time;
            assert!((-1e-9..=TICK + 1e-9).contains(&behind), "frame {step}: live {behind} s behind");
        }
    }

    /// At a steady 60 fps live runs one tick a frame, the same ticks as export.
    #[test]
    fn liquid_clock_steady_live_equals_offline() {
        let mut live = LiquidClock::default();
        let mut offline = LiquidClock::default();
        for i in 0..=120 {
            let t = i as f64 * TICK;
            let a = live.advance(t, TICK, 1.0, 0.0, false, false);
            let b = offline.advance(t, TICK, 1.0, 0.0, false, true);
            assert_eq!(a, b, "frame {i}");
        }
    }

    /// A coupled domain's cap: 0 holds without dropping the owed tick, 1 runs
    /// one; offline obeys the cap too and drops beyond one tick of debt.
    #[test]
    fn liquid_clock_tick_cap_holds_and_limits() {
        let mut clock = LiquidClock::default();
        clock.set_tick_cap(Some(1));
        clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        clock.set_tick_cap(Some(0));
        let held = clock.advance(TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!((held.ticks, held.dropped_seconds), (0, 0.0));
        clock.set_tick_cap(Some(1));
        let caught = clock.advance(2.0 * TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!((caught.ticks, caught.dropped_seconds), (1, 0.0));
        let offline = clock.advance(5.0 * TICK, 3.0 * TICK, 1.0, 0.0, false, true);
        assert_eq!(offline.ticks, 1);
        // Four due (one still owed from the catch-up): one runs, one stays
        // owed, two drop.
        assert!((offline.dropped_seconds - 2.0 * TICK).abs() < 1e-9);
        clock.set_tick_cap(None);
        let uncapped = clock.advance(8.0 * TICK, 3.0 * TICK, 1.0, 0.0, false, true);
        assert_eq!(uncapped.ticks, 4);
    }

    /// A state reset restarts once in a new epoch while the transport runs on.
    #[test]
    fn liquid_clock_restart_starts_a_new_epoch() {
        let mut clock = LiquidClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, true);
        let played = clock.advance(10.0 * TICK, 10.0 * TICK, 1.0, 0.0, false, true);
        clock.restart();
        let restarted = clock.advance(11.0 * TICK, TICK, 1.0, 0.0, false, true);
        assert!(restarted.restarted);
        assert_eq!(restarted.epoch, played.epoch + 1);
        assert_eq!((restarted.ticks, restarted.simulation_time), (0, 0.0));
        let next = clock.advance(12.0 * TICK, TICK, 1.0, 0.0, false, true);
        assert!(!next.restarted);
        assert_eq!((next.epoch, next.ticks), (restarted.epoch, 1));
    }
}
