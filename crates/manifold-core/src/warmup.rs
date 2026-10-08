//! Load-time warmup state shared across the content thread and UI.
//!
//! Warmup runs inside the `LoadProject` content-command handler: every
//! generator layer is built and rendered offscreen until its async work
//! (GLB parses, RT accel builds, model loads) quiesces. Progress is published
//! on the existing `ContentState` snapshot so the UI can draw a load bar.

/// Progress of the load-time warmup pass, published per layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WarmupProgress {
    pub done: u32,
    pub total: u32,
    pub label: String,
}

impl WarmupProgress {
    /// Fraction complete, clamped to 0.0..1.0.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

/// Which cap ended warmup for a layer.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum WarmupCap {
    /// Frame-count sanity bound tripped.
    PerLayerFrames,
    /// Per-layer wall-clock ceiling tripped.
    PerLayerWallClock,
    /// Whole-pass wall-clock ceiling tripped.
    TotalWallClock,
}

/// Per-layer and total budgets that bound warmup. Exhaustion logs and
/// continues — warmup never blocks a project from opening.
#[derive(Clone, Debug, Copy)]
pub struct WarmupBudget {
    /// Wall-clock ceiling for one layer before giving up.
    pub per_layer: std::time::Duration,
    /// Frame-count sanity bound for one layer (prevents a spin loop from
    /// outrunning the wall-clock cap indefinitely).
    pub per_layer_frames: u32,
    /// Live project frame interval. Pending preparation must not spin faster
    /// than playback; standalone callers use the default 60 Hz interval.
    pub frame_interval: std::time::Duration,
    /// Wall-clock ceiling for the whole pass.
    pub total: std::time::Duration,
}

impl Default for WarmupBudget {
    fn default() -> Self {
        Self {
            per_layer: std::time::Duration::from_secs(10),
            per_layer_frames: 600,
            frame_interval: std::time::Duration::from_secs_f64(1.0 / 60.0),
            total: std::time::Duration::from_secs(60),
        }
    }
}

impl WarmupBudget {
    /// Wall time is authoritative when both limits are reached together.
    pub fn exhausted(self, elapsed: std::time::Duration, frames: u32) -> Option<WarmupOutcome> {
        let cap = if elapsed >= self.per_layer {
            WarmupCap::PerLayerWallClock
        } else if frames >= self.per_layer_frames {
            WarmupCap::PerLayerFrames
        } else {
            return None;
        };
        Some(WarmupOutcome::BudgetExhausted { cap, elapsed })
    }

    /// Remaining wait after a pump that still has pending work. Call only
    /// after the existing quiescence query; pacing is not a readiness test.
    pub fn pending_pump_delay(
        self,
        elapsed: std::time::Duration,
        pump_elapsed: std::time::Duration,
    ) -> std::time::Duration {
        // A high-FPS project must not turn the spin guard into a shorter
        // wall-clock allowance. Round up so even a fake clock reaches the
        // wall limit by the final permitted pending pump.
        let frames = self.per_layer_frames.max(1);
        let mut guard_interval = self.per_layer / frames;
        if guard_interval * frames < self.per_layer {
            guard_interval += std::time::Duration::from_nanos(1);
        }
        self.frame_interval.max(guard_interval)
            .saturating_sub(pump_elapsed)
            .min(self.per_layer.saturating_sub(elapsed))
    }
}

/// Result of warming one layer.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum WarmupOutcome {
    /// Async work quiesced within budget.
    Quiescent,
    /// A budget cap was exhausted; the `cap` and `elapsed` fields say which
    /// one and for how long the layer was pumped. The layer may first-touch
    /// once at play.
    BudgetExhausted {
        cap: WarmupCap,
        elapsed: std::time::Duration,
    },
    /// Generator construction failed (registry rejected the type or the
    /// preset could not be loaded). Distinct from a budget trip — this layer
    /// will pay the same construction cost on stage and likely render black
    /// until the root cause is fixed.
    InstallFailed,
    /// GPU execution failed; callers must stop submitting and preserve the fault report.
    GpuFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct FakePendingPrimitive {
        ready_at: Option<Duration>,
    }

    impl FakePendingPrimitive {
        fn warmup_pending(&self, now: Duration) -> bool {
            self.ready_at.is_none_or(|ready_at| now < ready_at)
        }
    }

    // A deterministic clock exercises the same cap and pacing decisions as
    // the GPU pumps without waiting ten seconds or opening a GPU device.
    fn pump(primitive: FakePendingPrimitive, budget: WarmupBudget) -> (WarmupOutcome, u32) {
        let mut elapsed = Duration::ZERO;
        let work = Duration::from_micros(100);
        for frame in 0..budget.per_layer_frames {
            if let Some(outcome) = budget.exhausted(elapsed, frame) {
                return (outcome, frame);
            }
            elapsed += work;
            if !primitive.warmup_pending(elapsed) {
                return (WarmupOutcome::Quiescent, frame + 1);
            }
            elapsed += budget.pending_pump_delay(elapsed, work);
        }
        (budget.exhausted(elapsed, budget.per_layer_frames).unwrap(), budget.per_layer_frames)
    }

    #[test]
    fn two_second_background_load_quiesces_before_the_spin_guard() {
        let (outcome, frames) = pump(
            FakePendingPrimitive { ready_at: Some(Duration::from_secs(2)) },
            WarmupBudget::default(),
        );
        assert!(matches!(outcome, WarmupOutcome::Quiescent));
        assert!(frames < 600);
    }

    #[test]
    fn endless_background_load_reaches_wall_clock_at_all_project_rates() {
        for fps in [24.0, 30.0, 60.0, 120.0] {
            let budget = WarmupBudget {
                frame_interval: Duration::from_secs_f64(1.0 / fps),
                ..WarmupBudget::default()
            };
            let (outcome, _) = pump(FakePendingPrimitive { ready_at: None }, budget);
            assert!(matches!(outcome, WarmupOutcome::BudgetExhausted {
                cap: WarmupCap::PerLayerWallClock, elapsed,
            } if elapsed >= budget.per_layer && elapsed <= budget.per_layer + Duration::from_micros(100)),
                "fps={fps}: {outcome:?}");
        }
    }

    #[test]
    fn slow_pumps_and_expired_budgets_do_not_sleep() {
        let budget = WarmupBudget::default();
        assert_eq!(budget.pending_pump_delay(Duration::from_millis(50), Duration::from_millis(50)), Duration::ZERO);
        assert_eq!(budget.pending_pump_delay(budget.per_layer, Duration::ZERO), Duration::ZERO);
        assert_eq!(budget.pending_pump_delay(budget.per_layer - Duration::from_millis(1), Duration::ZERO), Duration::from_millis(1));
    }

    #[test]
    fn spin_guard_remains_distinct_from_quiescence_and_wall_clock() {
        let budget = WarmupBudget::default();
        assert!(matches!(budget.exhausted(Duration::ZERO, 600), Some(WarmupOutcome::BudgetExhausted {
            cap: WarmupCap::PerLayerFrames, ..
        })));
        assert!(matches!(pump(FakePendingPrimitive { ready_at: Some(Duration::ZERO) }, budget), (WarmupOutcome::Quiescent, 1)));
    }
}
