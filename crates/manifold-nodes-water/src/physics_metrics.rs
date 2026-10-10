//! Physics-side helpers for the per-frame simulation metrics.
//!
//! The metrics themselves live in the executor's slot
//! ([`manifold_node_engine::exec::sim_metrics`]); nodes record through
//! `ctx.sim_metrics`. This module adds what needs physics types: clock
//! records and the per-world discarded-time history.

use manifold_node_engine::exec::sim_metrics::{ClockRecord, SimMetrics};

/// Record one live clock's decisions for this frame. Offline clocks (export)
/// have no live cap and are not recorded.
pub fn record_clock(
    metrics: &mut SimMetrics,
    clock: &manifold_physics::clock::SimulationClock,
    frame: &manifold_physics::clock::ClockFrame,
    completed_ticks: Option<u64>,
) {
    let Some(live_cap) = frame.live_cap else { return };
    metrics.clock.push(ClockRecord {
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
        metrics: &mut SimMetrics,
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
        metrics.record_simulation_with_drop(
            target,
            completed,
            fresh_drop,
            cap_hit || fresh_drop > 0.0,
            nonfinite || !dropped_seconds.is_finite(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_drop_warns_then_clears_without_hiding_completion_lag() {
        let mut dropped = DroppedTimeTracker::default();
        let mut m = SimMetrics::default();
        dropped.record(&mut m, 2.0, 1.875, 0.5, false, false);
        let metrics = m.take();
        assert_eq!(metrics.backlog_seconds, 0.5);
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);

        dropped.record(&mut m, 2.0, 1.875, 0.5, false, false);
        let metrics = m.take();
        assert_eq!(metrics.backlog_seconds, 0.125);
        assert!(!metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);

        dropped.record(&mut m, 2.0, 2.0, 0.5, false, false);
        assert_eq!(m.take(), SimMetrics::default());

        dropped.record(&mut m, 2.0, 2.0, 0.75, false, false);
        let metrics = m.take();
        assert_eq!(metrics.backlog_seconds, 0.25);
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);
    }

    #[test]
    fn world_drop_histories_are_independent_and_completion_lag_can_dominate() {
        let mut first = DroppedTimeTracker::default();
        let mut second = DroppedTimeTracker::default();
        let mut m = SimMetrics::default();
        first.record(&mut m, 2.0, 1.0, 0.25, false, false);
        second.record(&mut m, 2.0, 2.0, 0.5, false, false);
        let metrics = m.take();
        assert_eq!(metrics.backlog_seconds, 1.0);
        assert!(metrics.sim_step_cap_hit);

        first.record(&mut m, 2.0, 2.0, 0.25, false, false);
        second.record(&mut m, 2.0, 2.0, 0.5, false, false);
        assert_eq!(m.take(), SimMetrics::default());
    }

    #[test]
    fn epoch_reset_clears_dropped_counter_history() {
        let mut dropped = DroppedTimeTracker::default();
        let mut m = SimMetrics::default();
        dropped.record(&mut m, 2.0, 2.0, 0.5, false, false);
        m.take();
        dropped.reset();

        dropped.record(&mut m, 0.0, 0.0, 0.0, false, false);
        assert_eq!(m.take(), SimMetrics::default());

        dropped.record(&mut m, 0.5, 0.5, 0.25, false, false);
        let metrics = m.take();
        assert_eq!(metrics.backlog_seconds, 0.25);
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);
    }

    #[test]
    fn cap_hit_is_advisory_and_nonfinite_diagnostics_survive() {
        let mut dropped = DroppedTimeTracker::default();
        let mut m = SimMetrics::default();
        dropped.record(&mut m, 1.0, 1.0, 0.0, true, false);
        let metrics = m.take();
        assert!(metrics.sim_step_cap_hit);
        assert!(!metrics.sim_nonfinite);
        assert_eq!(metrics.backlog_seconds, 0.0);

        dropped.record(&mut m, f64::NAN, 1.0, 0.0, false, false);
        assert!(m.take().sim_nonfinite);

        dropped.record(&mut m, 1.0, 1.0, f64::INFINITY, false, false);
        assert!(m.take().sim_nonfinite);

        dropped.record(&mut m, 1.0, 1.0, 0.0, false, true);
        assert!(m.take().sim_nonfinite);
    }

    #[test]
    fn each_live_clock_keeps_its_own_record() {
        use manifold_physics::clock::SimulationClock;
        let (mut a, mut b) = (SimulationClock::default(), SimulationClock::default());
        a.advance(0.0, 1.0 / 30.0, 1.0, 0.0, false, false);
        b.advance(0.0, 1.0 / 30.0, 1.0, 0.0, false, false);
        let mut m = SimMetrics::default();
        let restarted = a.advance(0.0, 1.0 / 30.0, 1.0, 1.0, false, false);
        record_clock(&mut m, &a, &restarted, Some(7));
        let ticked = b.advance(0.05, 1.0 / 30.0, 1.0, 0.0, false, false);
        record_clock(&mut m, &b, &ticked, None);
        let offline = b.advance(0.1, 1.0 / 30.0, 1.0, 0.0, false, true);
        record_clock(&mut m, &b, &offline, None);
        let clocks = m.take().clock;
        let [first, second] = clocks.records() else { panic!("two live records") };
        assert_ne!(first.id, second.id);
        assert!(first.restarted && first.accepted == 0 && first.completed_ticks == Some(7));
        assert!(!second.restarted && second.accepted == 1 && second.completed_ticks.is_none());
        assert_ne!(first.epoch, 0);
    }
}
