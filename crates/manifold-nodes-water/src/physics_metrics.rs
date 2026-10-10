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
    /// Maximum completion lag or freshly discarded time across worlds, in seconds.
    pub backlog_seconds: f32,
    pub sim_step_cap_hit: bool,
    pub sim_nonfinite: bool,
    /// The live simulation clocks' own decisions this frame.
    pub clock: ClockMetrics,
}

/// One live clock's decisions this frame, copied from its `ClockFrame`,
/// never inferred from timing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClockRecord {
    /// The clock's process-unique instance id (`SimulationClock::instance`).
    pub id: u64,
    /// Sim Rate intervals accepted this frame.
    pub accepted: u32,
    /// Boundaries transport crossed since the last accepted one. `due == 0`
    /// means no boundary arrived, so nothing could be accepted.
    pub due: u32,
    /// Live acceptance cap in force (1 after a late frame, else 2).
    pub live_cap: u32,
    /// Ticks accepted since the epoch began, through this frame.
    pub accepted_through: u64,
    /// Ticks whose GPU work is fenced complete in this epoch, where the
    /// domain tracks it (a liquid coupled to bodies).
    pub completed_ticks: Option<u64>,
    pub epoch: u32,
    pub transport: f64,
    pub restarted: bool,
    pub reanchored: bool,
    pub held: bool,
    /// Simulated seconds this frame's reanchor discarded.
    pub fresh_dropped_seconds: f64,
}

/// Fixed storage for the frame's clock records; no allocation.
pub const MAX_CLOCK_RECORDS: usize = 4;

/// Every live clock advanced this frame, one record each, in advance order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClockMetrics {
    records: [ClockRecord; MAX_CLOCK_RECORDS],
    len: u8,
    /// Intervals accepted by every live clock this frame, overflowed ones included.
    pub accepted_total: u32,
    /// Clocks advanced beyond the fixed storage; their records are lost.
    pub overflow: u32,
}

impl ClockMetrics {
    pub fn records(&self) -> &[ClockRecord] {
        &self.records[..usize::from(self.len)]
    }

    pub fn push(&mut self, record: ClockRecord) {
        self.accepted_total = self.accepted_total.saturating_add(record.accepted);
        match self.records.get_mut(usize::from(self.len)) {
            Some(slot) => {
                *slot = record;
                self.len += 1;
            }
            None => self.overflow = self.overflow.saturating_add(1),
        }
    }
}

const NO_RECORD: ClockRecord = ClockRecord {
    id: 0,
    accepted: 0,
    due: 0,
    live_cap: 0,
    accepted_through: 0,
    completed_ticks: None,
    epoch: 0,
    transport: 0.0,
    restarted: false,
    reanchored: false,
    held: false,
    fresh_dropped_seconds: 0.0,
};

const NO_CLOCK: ClockMetrics = ClockMetrics { records: [NO_RECORD; MAX_CLOCK_RECORDS], len: 0, accepted_total: 0, overflow: 0 };

thread_local! {
    static FRAME_METRICS: Cell<PhysicsMetrics> = const { Cell::new(PhysicsMetrics {
        physics_cpu_ms: 0.0,
        body_count: 0,
        backlog_seconds: 0.0,
        sim_step_cap_hit: false,
        sim_nonfinite: false,
        clock: NO_CLOCK,
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
            clock: current.clock,
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
    record_simulation_with_drop(target, completed, 0.0, cap_hit, nonfinite);
}

/// Record one live clock's decisions for this frame. Offline clocks (export)
/// have no live cap and are not recorded.
#[inline]
pub fn record_clock(
    clock: &manifold_physics::clock::SimulationClock,
    frame: &manifold_physics::clock::ClockFrame,
    completed_ticks: Option<u64>,
) {
    let Some(live_cap) = frame.live_cap else { return };
    if !RECORDING_ENABLED.with(Cell::get) {
        return;
    }
    FRAME_METRICS.with(|metrics| {
        let mut current = metrics.get();
        current.clock.push(ClockRecord {
            id: clock.instance(),
            accepted: frame.ticks,
            due: frame.due,
            live_cap,
            accepted_through: frame.first_sequence + u64::from(frame.ticks),
            completed_ticks,
            epoch: frame.epoch,
            transport: frame.transport,
            restarted: frame.restarted,
            reanchored: frame.reanchored,
            held: frame.held,
            fresh_dropped_seconds: frame.fresh_dropped_seconds,
        });
        metrics.set(current);
    });
}

/// Converts one world's cumulative discarded time into a frame-local advisory.
/// The owning state resets this history alongside its completion time on epoch reset.
#[derive(Default)]
pub struct DroppedTimeTracker {
    previous_seconds: f64,
}

impl DroppedTimeTracker {
    #[inline]
    pub fn reset(&mut self) {
        self.previous_seconds = 0.0;
    }

    #[inline]
    pub fn record(
        &mut self,
        target: f64,
        completed: f64,
        dropped_seconds: f64,
        cap_hit: bool,
        nonfinite: bool,
    ) {
        let fresh_drop = if dropped_seconds.is_finite() {
            let dropped_seconds = dropped_seconds.max(0.0);
            let fresh = (dropped_seconds - self.previous_seconds).max(0.0);
            self.previous_seconds = dropped_seconds;
            fresh
        } else {
            0.0
        };
        record_simulation_with_drop(
            target,
            completed,
            fresh_drop,
            cap_hit || fresh_drop > 0.0,
            nonfinite || !dropped_seconds.is_finite(),
        );
    }
}

#[inline]
fn record_simulation_with_drop(
    target: f64,
    completed: f64,
    fresh_drop: f64,
    cap_hit: bool,
    nonfinite: bool,
) {
    if !RECORDING_ENABLED.with(Cell::get) {
        return;
    }
    FRAME_METRICS.with(|metrics| {
        let mut current = metrics.get();
        let lag = target - completed;
        if lag.is_finite() {
            current.backlog_seconds = current.backlog_seconds.max(lag.max(0.0) as f32);
        }
        current.backlog_seconds = current.backlog_seconds.max(fresh_drop as f32);
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
    fn fresh_drop_warns_then_clears_without_hiding_completion_lag() {
        let mut dropped = DroppedTimeTracker::default();
        begin_frame();
        dropped.record(2.0, 1.875, 0.5, false, false);
        let metrics = take_frame();
        assert_eq!(metrics.backlog_seconds, 0.5);
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);

        begin_frame();
        dropped.record(2.0, 1.875, 0.5, false, false);
        let metrics = take_frame();
        assert_eq!(metrics.backlog_seconds, 0.125);
        assert!(!metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);

        begin_frame();
        dropped.record(2.0, 2.0, 0.5, false, false);
        assert_eq!(take_frame(), PhysicsMetrics::default());

        begin_frame();
        dropped.record(2.0, 2.0, 0.75, false, false);
        let metrics = take_frame();
        assert_eq!(metrics.backlog_seconds, 0.25);
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);
    }

    #[test]
    fn world_drop_histories_are_independent_and_completion_lag_can_dominate() {
        let mut first = DroppedTimeTracker::default();
        let mut second = DroppedTimeTracker::default();
        begin_frame();
        first.record(2.0, 1.0, 0.25, false, false);
        second.record(2.0, 2.0, 0.5, false, false);
        let metrics = take_frame();
        assert_eq!(metrics.backlog_seconds, 1.0);
        assert!(metrics.sim_step_cap_hit);

        begin_frame();
        first.record(2.0, 2.0, 0.25, false, false);
        second.record(2.0, 2.0, 0.5, false, false);
        assert_eq!(take_frame(), PhysicsMetrics::default());
    }

    #[test]
    fn epoch_reset_clears_dropped_counter_history() {
        let mut dropped = DroppedTimeTracker::default();
        begin_frame();
        dropped.record(2.0, 2.0, 0.5, false, false);
        take_frame();
        dropped.reset();

        begin_frame();
        dropped.record(0.0, 0.0, 0.0, false, false);
        assert_eq!(take_frame(), PhysicsMetrics::default());

        begin_frame();
        dropped.record(0.5, 0.5, 0.25, false, false);
        let metrics = take_frame();
        assert_eq!(metrics.backlog_seconds, 0.25);
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);
    }

    #[test]
    fn cap_hit_is_advisory_and_nonfinite_diagnostics_survive() {
        let mut dropped = DroppedTimeTracker::default();
        begin_frame();
        dropped.record(1.0, 1.0, 0.0, true, false);
        let metrics = take_frame();
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);
        assert_eq!(metrics.backlog_seconds, 0.0);

        begin_frame();
        dropped.record(f64::NAN, 1.0, 0.0, false, false);
        assert!(take_frame().sim_nonfinite);

        begin_frame();
        dropped.record(1.0, 1.0, f64::INFINITY, false, false);
        assert!(take_frame().sim_nonfinite);

        begin_frame();
        dropped.record(1.0, 1.0, 0.0, false, true);
        assert!(take_frame().sim_nonfinite);
    }

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
    fn each_live_clock_keeps_its_own_record() {
        use manifold_physics::clock::SimulationClock;
        let (mut a, mut b) = (SimulationClock::default(), SimulationClock::default());
        a.advance(0.0, 1.0 / 30.0, 1.0, 0.0, false, false);
        b.advance(0.0, 1.0 / 30.0, 1.0, 0.0, false, false);
        begin_frame();
        let restarted = a.advance(0.0, 1.0 / 30.0, 1.0, 1.0, false, false);
        record_clock(&a, &restarted, Some(7));
        let ticked = b.advance(0.05, 1.0 / 30.0, 1.0, 0.0, false, false);
        record_clock(&b, &ticked, None);
        let offline = b.advance(0.1, 1.0 / 30.0, 1.0, 0.0, false, true);
        record_clock(&b, &offline, None);
        let clocks = take_frame().clock;
        let [first, second] = clocks.records() else { panic!("two live records") };
        assert_ne!(first.id, second.id);
        assert!(first.restarted && first.accepted == 0 && first.completed_ticks == Some(7));
        assert!(!second.restarted && second.accepted == 1 && second.completed_ticks.is_none());
        assert_ne!(first.epoch, 0);
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
