//! Per-frame simulation metrics, written by simulation nodes and read by the
//! host after the frame.
//!
//! Each executor owns one [`SimMetrics`] slot, preallocated and plain
//! `Copy` data. Nodes record through the [`SimMetricsSink`] their context
//! carries; the host drains every executor it ran after the frame and merges
//! the results. A context with no sink (previews, thumbnails, standalone
//! tests) records nothing, so offscreen work never reaches the live HUD.

use std::cell::Cell;

/// Simulation work recorded during one frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SimMetrics {
    /// CPU time spent stepping simulations and extracting results, in milliseconds.
    pub cpu_ms: f32,
    /// Number of bodies evaluated across all simulations.
    pub body_count: u32,
    /// Maximum completion lag or freshly discarded time across worlds, in seconds.
    pub backlog_seconds: f32,
    pub sim_step_cap_hit: bool,
    pub sim_nonfinite: bool,
    /// The live simulation clocks' own decisions this frame.
    pub clock: ClockMetrics,
}

/// One live clock's decisions this frame, copied from its clock frame,
/// never inferred from timing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClockRecord {
    /// The clock's process-unique instance id.
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
    /// domain tracks it (a solver coupled to bodies).
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
        self.store(record);
    }

    fn store(&mut self, record: ClockRecord) {
        match self.records.get_mut(usize::from(self.len)) {
            Some(slot) => {
                *slot = record;
                self.len += 1;
            }
            None => self.overflow = self.overflow.saturating_add(1),
        }
    }

    fn merge(&mut self, other: &Self) {
        for record in other.records() {
            self.store(*record);
        }
        self.accepted_total = self.accepted_total.saturating_add(other.accepted_total);
        self.overflow = self.overflow.saturating_add(other.overflow);
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() { value.max(0.0) } else { 0.0 }
}

impl SimMetrics {
    /// Add one evaluated simulation world. World rebuild time is excluded by
    /// the caller.
    pub fn record_frame(&mut self, cpu_ms: f32, body_count: u32, pending_seconds: f32) {
        self.cpu_ms += finite_non_negative(cpu_ms);
        self.body_count = self.body_count.saturating_add(body_count);
        self.backlog_seconds = self.backlog_seconds.max(finite_non_negative(pending_seconds));
    }

    /// Add completed-time telemetry from a world without counting bodies or
    /// CPU cost. Submitted GPU endpoints are not completion.
    pub fn record_simulation(&mut self, target: f64, completed: f64, cap_hit: bool, nonfinite: bool) {
        self.record_simulation_with_drop(target, completed, 0.0, cap_hit, nonfinite);
    }

    pub fn record_simulation_with_drop(
        &mut self,
        target: f64,
        completed: f64,
        fresh_drop: f64,
        cap_hit: bool,
        nonfinite: bool,
    ) {
        let lag = target - completed;
        if lag.is_finite() {
            self.backlog_seconds = self.backlog_seconds.max(lag.max(0.0) as f32);
        }
        self.backlog_seconds = self.backlog_seconds.max(fresh_drop as f32);
        self.sim_step_cap_hit |= cap_hit;
        self.sim_nonfinite |= nonfinite || !lag.is_finite();
    }

    /// Fold another executor's frame into this one.
    pub fn merge(&mut self, other: &Self) {
        self.cpu_ms += other.cpu_ms;
        self.body_count = self.body_count.saturating_add(other.body_count);
        self.backlog_seconds = self.backlog_seconds.max(other.backlog_seconds);
        self.sim_step_cap_hit |= other.sim_step_cap_hit;
        self.sim_nonfinite |= other.sim_nonfinite;
        self.clock.merge(&other.clock);
    }

    /// Return the recorded frame and leave this slot cleared.
    pub fn take(&mut self) -> Self {
        std::mem::take(self)
    }
}

/// Where a node records this frame's simulation metrics: its executor's slot,
/// or nowhere.
#[derive(Clone, Copy, Default)]
pub struct SimMetricsSink<'a>(Option<&'a Cell<SimMetrics>>);

impl<'a> SimMetricsSink<'a> {
    pub const DISCARD: Self = Self(None);

    pub fn new(slot: &'a Cell<SimMetrics>) -> Self {
        Self(Some(slot))
    }

    #[inline]
    pub fn record(self, write: impl FnOnce(&mut SimMetrics)) {
        if let Some(slot) = self.0 {
            let mut metrics = slot.get();
            write(&mut metrics);
            slot.set(metrics);
        }
    }

    pub fn record_frame(self, cpu_ms: f32, body_count: u32, pending_seconds: f32) {
        self.record(|m| m.record_frame(cpu_ms, body_count, pending_seconds));
    }

    pub fn record_simulation(self, target: f64, completed: f64, cap_hit: bool, nonfinite: bool) {
        self.record(|m| m.record_simulation(target, completed, cap_hit, nonfinite));
    }

    pub fn merge(self, other: &SimMetrics) {
        self.record(|m| m.merge(other));
    }
}

impl std::fmt::Debug for SimMetricsSink<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "SimMetricsSink(slot)" } else { "SimMetricsSink(discard)" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_interval_metrics_use_completed_time_and_aggregate_warnings() {
        let mut metrics = SimMetrics::default();
        metrics.record_simulation(2.0, 1.875, true, false);
        metrics.record_simulation(8.0, 8.0, false, true);
        metrics.record_frame(1.0, 3, 0.05);
        assert_eq!(metrics.backlog_seconds, 0.125);
        assert!(metrics.sim_step_cap_hit && metrics.sim_nonfinite);
        assert_eq!(metrics.body_count, 3);
        metrics.take();
        metrics.record_simulation(0.0, 0.0, false, false);
        assert_eq!(metrics.take(), SimMetrics::default());
    }

    #[test]
    fn records_multiple_worlds_and_clears_on_take() {
        let mut metrics = SimMetrics::default();
        metrics.record_frame(1.25, 2, 0.25);
        metrics.record_frame(0.75, 3, 0.75);
        assert_eq!(
            metrics.take(),
            SimMetrics { cpu_ms: 2.0, body_count: 5, backlog_seconds: 0.75, ..SimMetrics::default() }
        );
        assert_eq!(metrics.take(), SimMetrics::default());
    }

    #[test]
    fn merge_matches_recording_into_one_slot() {
        let record = |id| ClockRecord { id, accepted: 2, ..ClockRecord::default() };
        let mut a = SimMetrics::default();
        a.record_frame(1.0, 2, 0.5);
        a.clock.push(record(1));
        let mut b = SimMetrics::default();
        b.record_simulation(1.0, 0.25, true, false);
        for id in 2..=4 {
            b.clock.push(record(id));
        }
        let mut merged = a;
        merged.merge(&b);
        assert_eq!(merged.cpu_ms, 1.0);
        assert_eq!(merged.body_count, 2);
        assert_eq!(merged.backlog_seconds, 0.75);
        assert!(merged.sim_step_cap_hit);
        let ids: Vec<u64> = merged.clock.records().iter().map(|r| r.id).collect();
        assert_eq!(ids, [1, 2, 3, 4]);
        assert_eq!(merged.clock.accepted_total, 8);
        merged.merge(&a);
        assert_eq!(merged.clock.overflow, 1);
        assert_eq!(merged.clock.accepted_total, 10);
    }

    #[test]
    fn discard_sink_records_nothing() {
        SimMetricsSink::DISCARD.record_frame(1.0, 1, 1.0);
        let slot = Cell::new(SimMetrics::default());
        SimMetricsSink::new(&slot).record_frame(1.0, 1, 1.0);
        assert_eq!(slot.get().body_count, 1);
    }
}
