//! The content tick's deadline grid (docs/FRAME_EXACT_TRANSPORT_DESIGN.md, D2).
//! Deadline n is `anchor + n × step`, and the frames due at a tick are counted
//! by comparing instants against those deadlines, never by dividing elapsed
//! time, so the count always agrees with the deadline the timer waited for.
//! Sub-frame lateness never changes the count; a missed deadline skips.
//!
//! The content thread's `FrameTimer` wraps this (P2). The step comes from
//! `transport_clock::step_for_fps`, so the grid and the transport agree.

use std::time::{Duration, Instant};

use crate::transport_clock::step_for_fps;

#[derive(Debug, Clone)]
pub struct FrameGrid {
    anchor: Instant,
    step: f64,
    /// The last deadline a tick consumed; deadline 0 is the anchor itself.
    consumed: i64,
}

impl FrameGrid {
    pub fn new(fps: f32, anchor: Instant) -> Self {
        let fps = if fps.is_finite() && fps > 0.0 { fps } else { 60.0 };
        Self { anchor, step: step_for_fps(fps), consumed: 0 }
    }

    /// Load and fps change: a fresh grid from `anchor`.
    pub fn reanchor(&mut self, fps: f32, anchor: Instant) {
        *self = Self::new(fps, anchor);
    }

    pub fn deadline(&self, n: i64) -> Instant {
        self.anchor + Duration::from_secs_f64(n.max(0) as f64 * self.step)
    }

    /// What the timer waits for before the next tick.
    pub fn next_deadline(&self) -> Instant {
        self.deadline(self.consumed + 1)
    }

    /// Frames to step at `now`: the deadlines passed since the last tick, at
    /// least 1. A tick before its deadline consumes that deadline early, so the
    /// following tick is not counted twice.
    pub fn consume_at(&mut self, now: Instant) -> u32 {
        let reached = self.last_deadline_at_or_before(now).max(self.consumed + 1);
        let frames = reached - self.consumed;
        self.consumed = reached;
        u32::try_from(frames).unwrap_or(u32::MAX)
    }

    /// The highest n with `deadline(n) <= now`. A division seeds the search;
    /// the instant comparisons decide.
    fn last_deadline_at_or_before(&self, now: Instant) -> i64 {
        let elapsed = now.saturating_duration_since(self.anchor).as_secs_f64();
        let mut n = ((elapsed / self.step) as i64).max(0);
        while n > 0 && self.deadline(n) > now {
            n -= 1;
        }
        while self.deadline(n + 1) <= now {
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn unit(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    const RATES: [f32; 6] = [60.0, 50.0, 24.0, 29.97, 23.976, 59.94];

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn grid_frames_ignore_subframe_jitter() {
        let mut rng = Rng(0x5EED);
        for fps in RATES {
            let step = step_for_fps(fps);
            // Random anchor phase: the grid has no relation to any wall second.
            let anchor = Instant::now() + secs(rng.unit() * step);
            let mut grid = FrameGrid::new(fps, anchor);
            for _ in 0..20_000 {
                // Production wakes at or after the deadline, late by under a frame.
                let now = grid.next_deadline() + secs(rng.unit() * 0.95 * step);
                assert_eq!(grid.consume_at(now), 1, "{fps}");
            }
            // A harness waking up to 40% early still counts one frame per tick.
            for _ in 0..20_000 {
                let offset = (rng.unit() * 0.8 - 0.4) * step;
                let deadline = grid.next_deadline();
                let now = if offset >= 0.0 { deadline + secs(offset) } else { deadline - secs(-offset) };
                assert_eq!(grid.consume_at(now), 1, "{fps} early or late wake");
            }
        }
    }

    #[test]
    fn grid_counts_missed_deadlines() {
        for fps in RATES {
            let step = step_for_fps(fps);
            let anchor = Instant::now();
            let mut grid = FrameGrid::new(fps, anchor);
            assert_eq!(grid.consume_at(grid.deadline(1)), 1);
            assert_eq!(grid.consume_at(grid.deadline(4) + secs(0.2 * step)), 3);
            // A 5 s stall, then an hour asleep.
            let stall = grid.deadline(4) + secs(5.0);
            let due = (5.0 * fps as f64).floor() as u32;
            let got = grid.consume_at(stall);
            assert!(got == due || got == due + 1, "{fps}: {got} vs {due}");
            let consumed = grid.consumed;
            let hour = grid.deadline(consumed) + secs(3600.0);
            let got = grid.consume_at(hour) as i64;
            let expected = (consumed..consumed + 300_000).take_while(|&n| grid.deadline(n + 1) <= hour).count() as i64;
            assert_eq!(got, expected, "{fps}: an hour of missed deadlines");
        }
    }

    /// The count equals a brute-force scan of deadlines at every boundary,
    /// near zero and two hours in: no division can disagree with the wait.
    #[test]
    fn grid_count_matches_deadline_comparison() {
        for fps in RATES {
            let anchor = Instant::now();
            for start in [0i64, 431_000] {
                for n in start..start + 3_000 {
                    for nudge in [-1i64, 0, 1] {
                        let mut grid = FrameGrid::new(fps, anchor);
                        grid.consumed = (n - 3).max(0);
                        let deadline = grid.deadline(n.max(1));
                        let now = match nudge {
                            -1 => deadline - Duration::from_nanos(1),
                            0 => deadline,
                            _ => deadline + Duration::from_nanos(1),
                        };
                        let mut brute = grid.consumed;
                        while grid.deadline(brute + 1) <= now {
                            brute += 1;
                        }
                        let expected = (brute - grid.consumed).max(1);
                        assert_eq!(grid.consume_at(now) as i64, expected, "{fps} n {n} nudge {nudge}");
                    }
                }
            }
        }
    }

    #[test]
    fn grid_and_clock_share_the_step() {
        use crate::transport_clock::TransportClock;
        for fps in RATES {
            let anchor = Instant::now();
            let grid = FrameGrid::new(fps, anchor);
            let clock = TransportClock::new(fps);
            for n in [1i64, 1_000, 432_000] {
                assert_eq!(grid.deadline(n) - anchor, Duration::from_secs_f64(clock.time_at(n).0));
            }
        }
    }
}
