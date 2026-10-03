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
    /// Transport paused or Simulation Speed 0 now. Impulses fired now are
    /// discarded, so resume never bursts.
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
    /// Speed over the interval after the last frame. Like every replayed
    /// control, a Speed edit takes effect from the frame that observes it.
    speed: f64,
    /// Transport and target where the current speed began (a start, a speed
    /// edit or a drop). Targets and tick starts are measured from here, so
    /// they never accumulate rounding and agree at every frame rate.
    anchor_transport: f64,
    anchor_target: f64,
}

impl LiquidClock {
    /// The transport time tick `tick` starts at, under the current speed.
    /// None before the clock starts or while Speed is 0.
    pub fn tick_start(&self, tick: u64) -> Option<f64> {
        (self.started && self.speed > 0.0)
            .then(|| self.anchor_transport + ((tick as f64) * TICK - self.anchor_target) / self.speed)
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
    /// transport time, ascending, with its transport time. A live drop moves
    /// the anchor, so an owed tick maps to a later transport and is visited
    /// again.
    pub fn tick_starts(&self, from: f64, until: f64, mut visit: impl FnMut(f64, u64)) {
        let mut tick = self.ticks_done;
        while let Some(transport) = self.tick_start(tick).filter(|&transport| transport <= until) {
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
        // Authored inputs were sampled at the last frame's target; within an
        // epoch the target never moves back past it, so a drop under live
        // overload can only give back this frame's advance.
        let mut floor = self.target_time;
        let speed = f64::from(speed);
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.dropped_seconds = 0.0;
            floor = 0.0;
            self.speed = speed;
            self.anchor_transport = transport;
            self.anchor_target = 0.0;
        } else {
            // The interval since the last frame ran at the last frame's speed.
            held = transport <= self.last_transport || speed <= 0.0;
            let reached = self.anchor_target + (transport - self.anchor_transport).max(0.0) * self.speed;
            self.target_time = reached.max(self.target_time);
            if speed != self.speed {
                self.speed = speed;
                self.anchor_transport = transport;
                self.anchor_target = self.target_time;
            }
        }
        self.last_transport = transport;
        let due = ((self.target_time / TICK + 1e-9).floor() as u64).saturating_sub(self.ticks_done);
        let ticks = if offline {
            due
        } else {
            let allowance = ((frame_interval / TICK) - 1e-6).ceil().clamp(1.0, f64::from(MAX_LIVE_TICKS)) as u64;
            let run = due.min(allowance);
            // Keep one tick of scheduling jitter; drop the rest visibly. What
            // the floor keeps stays owed and runs on later frames.
            let dropped = due.saturating_sub(run).saturating_sub(1);
            if dropped > 0 {
                let seconds = (dropped as f64 * TICK).min(self.target_time - floor).max(0.0);
                self.target_time -= seconds;
                self.dropped_seconds += seconds;
                self.anchor_transport = transport;
                self.anchor_target = self.target_time;
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
        // Speed 0.5 runs a tick every other frame and never holds. A speed
        // edit applies from the interval after the frame that sees it, so the
        // first of these frames still advances at speed 0.
        let frames: Vec<_> = (3..8)
            .map(|i| clock.advance(i as f64 * TICK, TICK, 0.5, 0.0, false, false))
            .collect();
        assert_eq!(frames.iter().map(|f| f.ticks).sum::<u32>(), 2);
        assert!(frames.iter().all(|f| !f.held));
        // A changed reset counter restarts in a new epoch.
        let reset = clock.advance(8.0 * TICK, TICK, 1.0, 1.0, false, false);
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

    /// The same live policy serves coupled and uncoupled domains.
    #[test]
    fn liquid_clock_live_24_30_60_fps_cover_the_same_ticks() {
        for fps in [24, 30, 60] {
            let mut clock = LiquidClock::default();
            let mut counts = Vec::new();
            for frame in 0..=fps {
                let out = clock.advance(frame as f64 / fps as f64, 1.0 / fps as f64, 1.0, 0.0, false, false);
                assert_eq!(out.dropped_seconds, 0.0);
                assert!(out.ticks <= MAX_LIVE_TICKS);
                counts.push(out.ticks);
            }
            assert_eq!(clock.ticks_done(), 60, "{fps} fps");
            if fps == 24 {
                assert_eq!(&counts[1..5], &[2, 3, 2, 3]);
            }
        }
    }

    /// Jitter and overload never move the sampled target backwards.
    #[test]
    fn liquid_clock_target_never_regresses_under_live_overload() {
        let mut clock = LiquidClock::default();
        let mut transport = 0.0;
        let intervals = [12.5, 8.5, 2.5, 0.07, 0.2, 1.9, 2.5, 0.1, 3.0, 0.05, 2.5, 2.5];
        let mut previous = clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        for step in 0..400 {
            let interval = TICK * intervals[step % intervals.len()];
            transport += interval;
            let frame = clock.advance(transport, interval, 1.0, 0.0, false, false);
            assert!(!frame.restarted, "frame {step}");
            assert!(frame.target_time >= previous.target_time - 1e-12, "frame {step}: target went back");
            assert!(frame.target_time <= transport + 1e-9, "frame {step}: target ran ahead of transport");
            assert!(frame.dropped_seconds >= previous.dropped_seconds, "frame {step}: dropped time shrank");
            previous = frame;
        }
        assert!(previous.dropped_seconds > 0.0, "overload must have dropped something");
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
