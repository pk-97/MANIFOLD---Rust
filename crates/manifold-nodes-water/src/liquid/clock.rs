//! Liquid adapters share the physics-layer transport clock.
pub use manifold_physics::clock::{ClockFrame, SimulationClock as LiquidClock};

/// Initial field-buffer reserve, not a time or quality cap.
pub const FIELD_RESERVE_INTERVALS: u32 = 3;

/// GPU_FLUID_SURFACE_DESIGN.md D10: the blend presenting display time `s`
/// between frames at `t_a` and `t_b`, and their span. Display time never
/// passes the newest frame; one frame (`t_a == t_b`) presents it fully.
pub(crate) fn display_blend(s: f64, t_a: f64, t_b: f64) -> (f32, f32) {
    let span = t_b - t_a;
    if span <= 0.0 {
        return (1.0, 0.0);
    }
    (((s - t_a) / span).clamp(0.0, 1.0) as f32, span as f32)
}

/// A whitewater particle's draw scale from its remaining lifetime: the last
/// 0.2 seconds shrink instead of leaving a full-sized particle until removal.
pub(crate) fn whitewater_fade(lifetime: f32) -> f32 {
    (lifetime / 0.2).clamp(0.0, 1.0).sqrt()
}

manifold_core::testkit_visible! {
/// Every node input that takes the accepted simulation interval in seconds,
/// as (node type, input). Each is fed by its liquid domain's
/// `interval_duration` output: the GPU FLIP builder wires them, the graph
/// loader adds the wire to graphs saved without it, and
/// `gpu_flip_builder_graphs_feed_every_interval_input` fails a shipped preset
/// that leaves one to a param. A param holds its 1/60 s default at every Sim
/// Rate, so a duration input left on its param runs at the wrong rate.
pub(crate) const INTERVAL_DURATION_INPUTS: [(&str, &str); 5] = [
    ("node.gpu_flip_step", "interval_duration"),
    ("node.matter_state", "interval_duration"),
    ("node.whitewater_step", "dt"),
    ("node.liquid_solid_distance", "tick_seconds"),
    ("node.whitewater_obstacle_source", "tick_seconds"),
];
}

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
    fn liquid_clock_live_caps_stalled_frame_and_reanchors() {
        let mut clock = LiquidClock::default();
        // 60 fps: one tick per frame after the first.
        let frames: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * TICK, TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[0].restarted);
        assert!(out[1..].iter().all(|f| f.ticks == 1 && !f.restarted));
        let stalled = clock.advance(10.0 * TICK + 1.0, TICK, 1.0, 0.0, false, false);
        assert_eq!(stalled.ticks, 2);
        assert!(stalled.reanchored);
        assert!((stalled.dropped_seconds - (1.0 - 2.0 * TICK)).abs() < 1e-12);
        assert!((stalled.plan.end.0 - stalled.plan.start.0 - 2.0 * TICK).abs() < 1e-12);
        assert_eq!(stalled.simulation_time, stalled.target_time);
        let next = clock.advance(11.0 * TICK + 1.0, TICK, 1.0, 0.0, false, false);
        assert_eq!(next.ticks, 1);
        assert!(!next.reanchored);
        assert_eq!(next.plan.start, stalled.plan.end);
        assert_eq!(next.dropped_seconds, stalled.dropped_seconds);
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
        let held = clock.advance(TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!(held.ticks, 0);
        assert!(held.held);
        assert_eq!(held.simulation_time, a.simulation_time);
        // Speed 0 holds while transport runs.
        assert!(clock.advance(2.0 * TICK, TICK, 0.0, 0.0, false, false).held);
        // Speed 0.5 runs each fixed transport tick at half duration. A speed
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
        for _ in 0..6 {
            let held = paused.advance(10.0 * TICK, TICK, 1.0, 0.0, false, false);
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

    /// Live plans contain at most two fixed intervals with contiguous simulation
    /// endpoints. Any excess transport is counted as discarded time.
    #[test]
    fn liquid_clock_live_plan_keeps_fixed_intervals_under_overload() {
        for fps in [20, 24, 30, 60] {
            let mut clock = LiquidClock::default();
            let mut previous_end = 0.0;
            let mut sequence = 0;
            for frame_index in 0..=fps {
                let transport = frame_index as f64 / fps as f64;
                let frame = clock.advance(
                    transport,
                    TICK,
                    1.0,
                    0.0,
                    false,
                    false,
                );
                assert_eq!(frame.plan.start.0, previous_end, "{fps} fps frame {frame_index}");
                assert!(frame.ticks <= 2);
                assert_eq!(frame.first_sequence, sequence);
                let mut endpoint = previous_end;
                for ordinal in 0..u64::from(frame.ticks) {
                    let interval = frame.interval(ordinal).expect("live frame interval");
                    assert_eq!(interval.start.0, endpoint);
                    assert!((interval.duration().0 - TICK).abs() < 1e-12);
                    endpoint = interval.end.0;
                }
                assert_eq!(endpoint, frame.plan.end.0);
                assert!((frame.target_time + frame.dropped_seconds - transport).abs() < 1e-12);
                assert!(transport - frame.simulation_time - frame.dropped_seconds < TICK + 1e-12);
                sequence += u64::from(frame.ticks);
                previous_end = frame.plan.end.0;
            }
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

    /// At 20/24 fps overload discards the excess. At 30/60 fps the two-step cap
    /// covers the same second as export without dropping time.
    #[test]
    fn liquid_clock_live_20_24_30_60_fps_account_for_accepted_and_dropped_time() {
        for (fps, expected_ticks) in [(20, 40), (24, 48), (30, 60), (60, 60)] {
            let mut clock = LiquidClock::default();
            let mut completed = 0.0;
            let mut dropped = 0.0;
            for frame in 0..=fps {
                let out = clock.advance(frame as f64 / fps as f64, TICK, 1.0, 0.0, false, false);
                assert!(out.ticks <= 2);
                assert_eq!(out.plan.start.0, completed);
                completed = out.plan.end.0;
                dropped = out.dropped_seconds;
                assert!((out.target_time + dropped - frame as f64 / fps as f64).abs() < 1e-12);
            }
            assert_eq!(clock.ticks_done(), expected_ticks, "{fps} fps");
            assert!((completed - expected_ticks as f64 * TICK).abs() < 1e-12);
            assert!((completed + dropped - 1.0).abs() < 1e-12, "{fps} fps");
        }
    }

    /// Live pacing, frame by frame: every display frame accepts at most two
    /// fixed 60 Hz intervals, so 20 and 24 fps run two a frame and drop the
    /// rest, 30 fps runs two and 60 fps one, and each frame ends on the
    /// boundary of the intervals it accepted.
    #[test]
    fn liquid_clock_live_accepts_at_most_two_intervals_per_frame() {
        for fps in [20u32, 24, 30, 60] {
            let mut clock = LiquidClock::default();
            let ticks_per_frame = (60 / fps).min(2);
            clock.advance(0.0, TICK, 1.0, 0.0, false, false);
            for frame in 1..=fps {
                let out = clock.advance(f64::from(frame) / f64::from(fps), TICK, 1.0, 0.0, false, false);
                assert_eq!(out.ticks, ticks_per_frame, "{fps} fps frame {frame}: accepted interval count");
                let boundary = f64::from(frame * ticks_per_frame) * TICK;
                assert!((out.simulation_time - boundary).abs() < 1e-9, "{fps} fps frame {frame}: lost simulation time");
            }
        }
    }

    /// Jitter preserves contiguous fixed steps and a monotone target while
    /// discarded transport is accounted for separately from accepted time.
    #[test]
    fn liquid_clock_target_never_regresses_under_live_jitter() {
        let mut clock = LiquidClock::default();
        let mut transport = 0.0;
        let intervals = [12.5, 8.5, 2.5, 0.07, 0.2, 1.9, 2.5, 0.1, 3.0, 0.05, 2.5, 2.5];
        let mut previous = clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        for step in 0..400 {
            let interval = TICK * intervals[step % intervals.len()];
            transport += interval;
            let frame = clock.advance(transport, TICK, 1.0, 0.0, false, false);
            assert!(!frame.restarted, "frame {step}");
            assert!(frame.target_time >= previous.target_time - 1e-12, "frame {step}: target went back");
            assert!(frame.target_time <= transport + 1e-9, "frame {step}: target ran ahead of transport");
            assert_eq!(frame.plan.start, previous.plan.end);
            assert!(frame.ticks <= 2, "frame {step}: live cap");
            assert!((frame.plan.end.0 - frame.plan.start.0 - f64::from(frame.ticks) * TICK).abs() < 1e-12);
            assert!(frame.dropped_seconds >= previous.dropped_seconds);
            if frame.reanchored {
                assert_eq!(frame.ticks, 2);
                assert!(frame.dropped_seconds > previous.dropped_seconds);
            }
            assert!((frame.target_time + frame.dropped_seconds - transport).abs() < 1e-9);
            assert!(frame.plan.end.0 <= transport + 1e-9, "frame {step}: plan ran ahead");
            previous = frame;
        }
        assert!(previous.dropped_seconds > 0.0);
        assert!(transport - previous.simulation_time - previous.dropped_seconds < TICK + 1e-9);
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
