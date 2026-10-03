//! Per-content-frame CPU metrics for physics-world evaluations.
//!
//! Physics worlds are evaluated on the content thread, so a thread-local
//! accumulator keeps the hot path free of locks and per-frame allocations.
//! The content frame resets the accumulator before live rendering and takes it
//! once rendering completes.  Calls from multiple physics worlds therefore
//! contribute to the same displayed frame.

use std::cell::Cell;

/// Physics work recorded during one live content frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicsMetrics {
    /// CPU time spent stepping worlds and extracting poses, in milliseconds.
    pub physics_cpu_ms: f32,
    /// Number of bodies evaluated across all physics worlds.
    pub body_count: u32,
    /// Maximum unprocessed physics time across worlds, in seconds.
    pub backlog_seconds: f32,
    pub sim_step_cap_hit: bool,
    pub sim_nonfinite: bool,
}

thread_local! {
    static FRAME_METRICS: Cell<PhysicsMetrics> = const { Cell::new(PhysicsMetrics {
        physics_cpu_ms: 0.0,
        body_count: 0,
        backlog_seconds: 0.0,
        sim_step_cap_hit: false,
        sim_nonfinite: false,
    }) };
    static RECORDING_ENABLED: Cell<bool> = const { Cell::new(true) };
}

/// Temporarily suppress recording for an offscreen render nested inside the
/// live content frame, such as a parked clip thumbnail.
///
/// The guard is stack-only and restores the previous state on drop, so nested
/// thumbnail or preview paths cannot accidentally re-enable an outer guard.
#[must_use]
pub struct RecordingGuard {
    previous: bool,
}

impl Drop for RecordingGuard {
    fn drop(&mut self) {
        RECORDING_ENABLED.with(|enabled| enabled.set(self.previous));
    }
}

/// Suspend physics metrics recording until the returned guard is dropped.
#[inline]
pub fn suspend_recording() -> RecordingGuard {
    let previous = RECORDING_ENABLED.with(|enabled| {
        let previous = enabled.get();
        enabled.set(false);
        previous
    });
    RecordingGuard { previous }
}

/// Clear metrics before beginning a live content frame.
#[inline]
pub fn begin_frame() {
    FRAME_METRICS.with(|metrics| metrics.set(PhysicsMetrics::default()));
    RECORDING_ENABLED.with(|enabled| enabled.set(true));
}

/// Add one successfully evaluated physics world to the current frame.
///
/// The caller supplies timing for stepping and pose extraction. World rebuild
/// time is intentionally excluded by the physics-world evaluator.
#[inline]
pub fn record_frame(physics_ms: f32, body_count: u32, pending_seconds: f32) {
    if !RECORDING_ENABLED.with(Cell::get) {
        return;
    }
    FRAME_METRICS.with(|metrics| {
        let current = metrics.get();
        metrics.set(PhysicsMetrics {
            physics_cpu_ms: current.physics_cpu_ms
                + if physics_ms.is_finite() {
                    physics_ms.max(0.0)
                } else {
                    0.0
                },
            body_count: current.body_count.saturating_add(body_count),
            sim_step_cap_hit: current.sim_step_cap_hit,
            sim_nonfinite: current.sim_nonfinite,
            backlog_seconds: current.backlog_seconds.max(if pending_seconds.is_finite() {
                pending_seconds.max(0.0)
            } else {
                0.0
            }),
        });
    });
}

/// Add completed-time telemetry from a liquid world without double-counting
/// rigid bodies or their CPU cost. Submitted GPU endpoints are not completion.
#[inline]
pub fn record_simulation(target: f64, completed: f64, cap_hit: bool, nonfinite: bool) {
    if !RECORDING_ENABLED.with(Cell::get) {
        return;
    }
    FRAME_METRICS.with(|metrics| {
        let mut current = metrics.get();
        let lag = target - completed;
        if lag.is_finite() {
            current.backlog_seconds = current.backlog_seconds.max(lag.max(0.0) as f32);
        }
        current.sim_step_cap_hit |= cap_hit;
        current.sim_nonfinite |= nonfinite || !lag.is_finite();
        metrics.set(current);
    });
}

/// Take the accumulated metrics and reset them for the next frame.
#[inline]
pub fn take_frame() -> PhysicsMetrics {
    FRAME_METRICS.with(|metrics| {
        let current = metrics.get();
        metrics.set(PhysicsMetrics::default());
        current
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_interval_metrics_use_completed_time_and_aggregate_warnings() {
        begin_frame();
        record_simulation(2.0, 1.875, true, false);
        record_simulation(8.0, 8.0, false, true);
        record_frame(1.0, 3, 0.05);
        let metrics = take_frame();
        assert_eq!(metrics.backlog_seconds, 0.125);
        assert!(metrics.sim_step_cap_hit && metrics.sim_nonfinite);
        assert_eq!(metrics.body_count, 3);
        begin_frame();
        record_simulation(0.0, 0.0, false, false);
        assert_eq!(take_frame(), PhysicsMetrics::default());
    }

    #[test]
    fn records_multiple_worlds_and_resets_on_take() {
        begin_frame();
        record_frame(1.25, 2, 0.25);
        record_frame(0.75, 3, 0.75);

        assert_eq!(
            take_frame(),
            PhysicsMetrics {
                physics_cpu_ms: 2.0,
                body_count: 5,
                backlog_seconds: 0.75,
                ..PhysicsMetrics::default()
            }
        );
        assert_eq!(take_frame(), PhysicsMetrics::default());
    }

    #[test]
    fn begin_frame_discards_previous_accumulator() {
        record_frame(4.0, 7, 1.0);
        begin_frame();

        assert_eq!(take_frame(), PhysicsMetrics::default());
    }

    #[test]
    fn suspended_recording_restores_nested_state() {
        begin_frame();
        let outer = suspend_recording();
        record_frame(1.0, 1, 1.0);
        {
            let _inner = suspend_recording();
            record_frame(2.0, 2, 2.0);
        }
        record_frame(4.0, 4, 4.0);
        drop(outer);
        record_frame(8.0, 8, 8.0);

        assert_eq!(
            take_frame(),
            PhysicsMetrics {
                physics_cpu_ms: 8.0,
                body_count: 8,
                backlog_seconds: 8.0,
                ..PhysicsMetrics::default()
            }
        );
    }
}
