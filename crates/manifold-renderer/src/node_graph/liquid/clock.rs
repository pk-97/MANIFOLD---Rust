//! Liquid adapters share the physics-layer transport clock.
pub use manifold_physics::clock::{ClockFrame, SimulationClock as LiquidClock};

/// Initial field-buffer reserve, not a time or quality cap.
pub const FIELD_RESERVE_INTERVALS: u32 = 3;
#[cfg(test)]
mod tests {
    use super::*;
    use manifold_physics::clock::TICK;

    fn run(clock: &mut LiquidClock, frames: &[(f64, f64)], offline: bool) -> Vec<ClockFrame> {
        frames
            .iter()
            .map(|&(t, dt)| clock.advance(t, dt, 1.0, 0.0, false, offline))
            .collect()
    }

    #[test]
    fn liquid_clock_live_covers_stalled_frame() {
        let mut clock = LiquidClock::default();
        // 60 fps: one tick per frame after the first.
        let frames: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * TICK, TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[0].restarted);
        assert!(out[1..].iter().all(|f| f.ticks == 1 && !f.restarted));
        let stalled = clock.advance(10.0 * TICK + 1.0, 1.0, 1.0, 0.0, false, false);
        assert_eq!(stalled.ticks, 1);
        assert_eq!(stalled.dropped_seconds, 0.0);
        assert_eq!(stalled.duration().0, 1.0);
        assert_eq!(stalled.simulation_time, stalled.target_time);
    }

    #[test]
    fn liquid_clock_export_runs_every_project_interval() {
        let mut clock = LiquidClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, true);
        let frame = clock.advance(1.0, TICK, 1.0, 0.0, false, true);
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
        assert_eq!(frames.iter().map(|f| f.ticks).sum::<u32>(), 4);
        assert!((frames.iter().map(|f| f.duration().0).sum::<f64>() - 2.0*TICK).abs() < 1e-12);
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

    /// Live frames expose the complete observed transport span. The ordinal
    /// identifies the one interval in each display frame; it is not a tick
    /// duration or a nominal fixed-tick budget.
    #[test]
    fn liquid_clock_live_plan_covers_observed_span() {
        for fps in [20, 24, 30, 60] {
            let mut clock = LiquidClock::default();
            let mut previous_end = 0.0;
            for frame_index in 0..=fps {
                let transport = frame_index as f64 / fps as f64;
                let frame = clock.advance(
                    transport,
                    1.0 / fps as f64,
                    1.0,
                    0.0,
                    false,
                    false,
                );
                assert_eq!(frame.plan.start.0, previous_end, "{fps} fps frame {frame_index}");
                assert_eq!(frame.plan.end.0, transport, "{fps} fps frame {frame_index}");
                assert_eq!(frame.dropped_seconds, 0.0);
                if frame.ticks == 1 {
                    assert_eq!(frame.first_sequence, frame_index as u64 - 1);
                    let interval = frame.interval(0).expect("live frame interval");
                    assert_eq!(interval.start.0, previous_end);
                    assert_eq!(interval.end.0, transport);
                }
                previous_end = frame.plan.end.0;
            }
            assert_eq!(previous_end, 1.0, "{fps} fps");
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
            assert_eq!(a.simulation_time.to_bits(), b.simulation_time.to_bits(), "frame {i}");
        }
    }

    /// The same live policy serves coupled and uncoupled domains.
    #[test]
    fn liquid_clock_live_20_24_30_60_fps_cover_the_same_time() {
        for fps in [20, 24, 30, 60] {
            let mut clock = LiquidClock::default();
            let mut completed = 0.0;
            for frame in 0..=fps {
                let out = clock.advance(frame as f64 / fps as f64, 1.0 / fps as f64, 1.0, 0.0, false, false);
                assert_eq!(out.dropped_seconds, 0.0);
                assert!(out.ticks <= 1);
                assert_eq!(out.plan.start.0, completed);
                completed = out.plan.end.0;
                assert_eq!(completed, frame as f64 / fps as f64);
            }
            assert_eq!(completed, 1.0, "{fps} fps");
        }
    }

    /// Jitter never moves the sampled target backwards or leaves the accepted
    /// live plan behind the observed transport.
    #[test]
    fn liquid_clock_target_never_regresses_under_live_jitter() {
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
            assert_eq!(frame.dropped_seconds, 0.0, "frame {step}: live time was dropped");
            assert!(frame.plan.end.0 <= transport + 1e-9, "frame {step}: plan ran ahead");
            previous = frame;
        }
        assert_eq!(previous.dropped_seconds, 0.0);
        assert_eq!(previous.simulation_time, transport);
    }

    /// A state reset restarts once in a new epoch while the transport runs on.
    #[test]
    fn liquid_clock_restart_starts_a_new_epoch() {
        let mut clock = LiquidClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, true);
        let played = clock.advance(10.0 * TICK, TICK, 1.0, 0.0, false, true);
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
