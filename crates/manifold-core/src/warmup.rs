//! Load-time warmup state shared across the content thread and UI.
//!
//! Warmup runs inside the `LoadProject` content-command handler: every
//! generator layer is built and rendered offscreen until its async work
//! (GLB parses, RT accel builds, model loads) quiesces. Progress is published
//! on the existing `ContentState` snapshot so the UI can draw a load bar.
//!
//! A pass owns one absolute deadline and one absolute deadline per layer,
//! started before construction and reused by all nested pumps and drains.
//! Every wait is clipped to both deadlines. Synchronous GPU work and scheduler
//! overshoot are the only soft-cap exceptions; nested work never renews a budget.

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
        self.frame_interval
            .max(guard_interval)
            .saturating_sub(pump_elapsed)
            .min(self.per_layer.saturating_sub(elapsed))
    }
}

/// Deadline ledger for one warmup pass. Revisiting a layer never renews its budget.
pub struct WarmupPass {
    run: WarmupRun,
    layers: std::collections::HashMap<crate::LayerId, WarmupRun>,
    master: Option<WarmupRun>,
    pub budget_exhausted: bool,
    pub install_failed: bool,
    pub preparation_failed: bool,
}

impl WarmupPass {
    pub fn new(budget: WarmupBudget, now: std::time::Instant) -> Self {
        Self {
            run: WarmupRun {
                budget,
                pass_start: now,
                pass_deadline: now + budget.total,
                layer_start: now,
                layer_deadline: now + budget.total,
            },
            layers: std::collections::HashMap::new(),
            master: None,
            budget_exhausted: false,
            install_failed: false,
            preparation_failed: false,
        }
    }

    /// Preserve independent failures when later topology/group outcomes replace earlier ones.
    /// GPU failures remain immediate aborts at the caller.
    pub fn record_outcome(&mut self, outcome: WarmupOutcome) {
        self.budget_exhausted |= matches!(outcome, WarmupOutcome::BudgetExhausted { .. });
        self.install_failed |= outcome == WarmupOutcome::InstallFailed;
        self.preparation_failed |= outcome == WarmupOutcome::PreparationFailed;
    }

    /// Pass-wide work before any layer has started.
    pub fn run(&self) -> WarmupRun {
        self.run
    }

    pub fn layer(&mut self, id: &crate::LayerId, now: std::time::Instant) -> WarmupRun {
        *self
            .layers
            .entry(id.clone())
            .or_insert_with(|| self.run.for_layer(now))
    }

    pub fn master(&mut self, now: std::time::Instant) -> WarmupRun {
        *self.master.get_or_insert_with(|| self.run.for_layer(now))
    }
}

/// Copyable deadlines passed unchanged through a layer's nested work.
#[derive(Clone, Copy, Debug)]
pub struct WarmupRun {
    pub budget: WarmupBudget,
    pass_start: std::time::Instant,
    pass_deadline: std::time::Instant,
    layer_start: std::time::Instant,
    layer_deadline: std::time::Instant,
}

impl WarmupRun {
    fn for_layer(self, now: std::time::Instant) -> Self {
        Self {
            layer_start: now,
            layer_deadline: now + self.budget.per_layer,
            ..self
        }
    }

    pub fn elapsed(self, now: std::time::Instant) -> std::time::Duration {
        now.saturating_duration_since(self.layer_start)
    }

    pub fn exhausted(self, now: std::time::Instant, frames: u32) -> Option<WarmupOutcome> {
        let (cap, elapsed) = if now >= self.pass_deadline {
            (
                WarmupCap::TotalWallClock,
                now.saturating_duration_since(self.pass_start),
            )
        } else if now >= self.layer_deadline {
            (WarmupCap::PerLayerWallClock, self.elapsed(now))
        } else if frames >= self.budget.per_layer_frames {
            (WarmupCap::PerLayerFrames, self.elapsed(now))
        } else {
            return None;
        };
        Some(WarmupOutcome::BudgetExhausted { cap, elapsed })
    }

    pub fn clamp_wait(
        self,
        now: std::time::Instant,
        wait: std::time::Duration,
    ) -> std::time::Duration {
        wait.min(self.pass_deadline.saturating_duration_since(now))
            .min(self.layer_deadline.saturating_duration_since(now))
    }

    pub fn pending_pump_delay(
        self,
        now: std::time::Instant,
        pump_elapsed: std::time::Duration,
    ) -> std::time::Duration {
        self.clamp_wait(
            now,
            self.budget
                .pending_pump_delay(self.elapsed(now), pump_elapsed),
        )
    }

    /// None means there is async work to pace. Successful quiescence still
    /// requires an installed runtime, no pending work and a presentable frame.
    pub fn pending_outcome(
        self,
        pending: bool,
        presentable: bool,
        installed: bool,
    ) -> Option<WarmupOutcome> {
        if !installed {
            Some(WarmupOutcome::InstallFailed)
        } else if pending {
            None
        } else if presentable {
            Some(WarmupOutcome::Quiescent)
        } else {
            Some(WarmupOutcome::PreparationFailed)
        }
    }
}

/// Result of warming one layer.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum WarmupOutcome {
    /// Async work quiesced within budget.
    Quiescent,
    /// A budget cap was exhausted. `elapsed` includes construction and is
    /// measured from pass start for TotalWallClock, or layer start otherwise.
    /// The layer may first-touch once at play.
    BudgetExhausted {
        cap: WarmupCap,
        elapsed: std::time::Duration,
    },
    /// Generator construction failed (registry rejected the type or the
    /// preset could not be loaded). Distinct from a budget trip — this layer
    /// will pay the same construction cost on stage and likely render black
    /// until the root cause is fixed.
    InstallFailed,
    /// Preparation ended without a presentable frame and nothing remains pending.
    PreparationFailed,
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
        let start = std::time::Instant::now();
        let run = WarmupPass::new(budget, start).layer(&crate::LayerId::new("test"), start);
        let mut elapsed = Duration::ZERO;
        let work = Duration::from_micros(100);
        for frame in 0..budget.per_layer_frames {
            if let Some(outcome) = run.exhausted(start + elapsed, frame) {
                return (outcome, frame);
            }
            elapsed += work;
            if let Some(outcome) =
                run.pending_outcome(primitive.warmup_pending(elapsed), true, true)
            {
                return (outcome, frame + 1);
            }
            elapsed += run.pending_pump_delay(start + elapsed, work);
        }
        (
            run.exhausted(start + elapsed, budget.per_layer_frames)
                .unwrap(),
            budget.per_layer_frames,
        )
    }

    #[test]
    fn nested_renderer_chain_topology_and_drains_share_pass_deadline() {
        let start = std::time::Instant::now();
        let budget = WarmupBudget::default();
        let mut pass = WarmupPass::new(budget, start);
        let id = crate::LayerId::new("nested");
        let mut now = start + Duration::from_secs(59);
        let renderer = pass.layer(&id, now);
        // Construction belongs to the same allowance as every later pump.
        now += Duration::from_millis(300);
        now += renderer.pending_pump_delay(now, Duration::ZERO);
        let chain = pass.layer(&id, now);
        now += chain.clamp_wait(now, Duration::from_millis(300));
        let topology = pass.layer(&id, now);
        now += topology.clamp_wait(now, Duration::from_secs(10));
        assert_eq!(now, start + budget.total);
        assert!(
            matches!(topology.exhausted(now, 0), Some(WarmupOutcome::BudgetExhausted {
            cap: WarmupCap::TotalWallClock, elapsed,
        }) if elapsed == budget.total)
        );
        for run in [renderer, chain, topology, pass.layer(&id, now), pass.run()] {
            assert_eq!(run.clamp_wait(now, Duration::from_secs(2)), Duration::ZERO);
            assert_eq!(run.pending_pump_delay(now, Duration::ZERO), Duration::ZERO);
        }
    }

    #[test]
    fn layer_deadline_includes_construction_and_is_not_renewed() {
        let start = std::time::Instant::now();
        let mut pass = WarmupPass::new(WarmupBudget::default(), start);
        let id = crate::LayerId::new("layer");
        let run = pass.layer(&id, start);
        let now = start + Duration::from_secs(9);
        let nested = pass.layer(&id, now);
        assert_eq!(
            nested.clamp_wait(now, Duration::from_secs(20)),
            Duration::from_secs(1)
        );
        assert_eq!(run.elapsed(now), nested.elapsed(now));
        assert!(matches!(
            nested.exhausted(start + Duration::from_secs(10), 0),
            Some(WarmupOutcome::BudgetExhausted {
                cap: WarmupCap::PerLayerWallClock,
                ..
            })
        ));
    }

    #[test]
    fn terminal_failure_returns_without_advancing_the_clock() {
        let start = std::time::Instant::now();
        let run = WarmupPass::new(WarmupBudget::default(), start).run();
        let mut now = start;
        let outcome = if let Some(outcome) = run.pending_outcome(false, false, true) {
            outcome
        } else {
            now += run.pending_pump_delay(now, Duration::ZERO);
            WarmupOutcome::Quiescent
        };
        assert_eq!(outcome, WarmupOutcome::PreparationFailed);
        assert_eq!(now, start);
        assert_eq!(
            run.pending_outcome(false, true, true),
            Some(WarmupOutcome::Quiescent)
        );
        assert_eq!(run.pending_outcome(true, false, true), None);
        assert_eq!(
            run.pending_outcome(false, true, false),
            Some(WarmupOutcome::InstallFailed)
        );
    }

    #[test]
    fn two_second_background_load_quiesces_before_the_spin_guard() {
        let (outcome, frames) = pump(
            FakePendingPrimitive {
                ready_at: Some(Duration::from_secs(2)),
            },
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
            assert!(
                matches!(outcome, WarmupOutcome::BudgetExhausted {
                cap: WarmupCap::PerLayerWallClock, elapsed,
            } if elapsed >= budget.per_layer && elapsed <= budget.per_layer + Duration::from_micros(100)),
                "fps={fps}: {outcome:?}"
            );
        }
    }

    #[test]
    fn slow_pumps_and_expired_budgets_do_not_sleep() {
        let budget = WarmupBudget::default();
        assert_eq!(
            budget.pending_pump_delay(Duration::from_millis(50), Duration::from_millis(50)),
            Duration::ZERO
        );
        assert_eq!(
            budget.pending_pump_delay(budget.per_layer, Duration::ZERO),
            Duration::ZERO
        );
        assert_eq!(
            budget.pending_pump_delay(budget.per_layer - Duration::from_millis(1), Duration::ZERO),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn spin_guard_remains_distinct_from_quiescence_and_wall_clock() {
        let budget = WarmupBudget::default();
        assert!(matches!(
            budget.exhausted(Duration::ZERO, 600),
            Some(WarmupOutcome::BudgetExhausted {
                cap: WarmupCap::PerLayerFrames,
                ..
            })
        ));
        assert!(matches!(
            pump(
                FakePendingPrimitive {
                    ready_at: Some(Duration::ZERO)
                },
                budget
            ),
            (WarmupOutcome::Quiescent, 1)
        ));
    }
}
