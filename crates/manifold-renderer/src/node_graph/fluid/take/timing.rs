//! Retained recording-clock provenance for fluid takes.
//!
//! The capture owns the same growing history used by the simulation
//! adapters. It records the clock observed by the host and only
//! advances its acknowledged boundary after the worker accepts a matching
//! handoff.

use manifold_core::{Beats, Seconds};
use manifold_physics::input::{InputHistory, Timestamped};

/// One project-time observation belonging to a recorded take.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeTime {
    pub beat: Beats,
    pub transport: Seconds,
    pub simulation: Seconds,
}

impl Timestamped for TakeTime {
    fn time(&self) -> Seconds {
        self.transport
    }
}

/// A project-time interval represented by two recorded clock observations.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeRange {
    pub start: TakeTime,
    pub end: TakeTime,
}

/// Clock observations handed to the fluid worker.
#[derive(Default)]
pub(in crate::node_graph::fluid) struct TimingHandoff {
    pub sequence: u64,
    pub points: Vec<TakeTime>,
    pub metadata_only: bool,
}

/// Retained host capture of the recording clock.
pub(in crate::node_graph::fluid) struct Capture {
    history: InputHistory<TakeTime>,
    spare: Option<Vec<TakeTime>>,
    observed_sequence: u64,
    acknowledged_sequence: u64,
    acknowledged_time: Option<Seconds>,
}

impl Capture {
    pub(in crate::node_graph::fluid) fn new(capacity: usize) -> Self {
        assert!(
            capacity >= 2,
            "fluid take timing history capacity must be at least two"
        );
        Self {
            history: InputHistory::with_growing_capacity(capacity)
                .expect("fluid take timing history capacity must be at least two"),
            spare: Some(Vec::with_capacity(capacity)),
            observed_sequence: 0,
            acknowledged_sequence: 0,
            acknowledged_time: None,
        }
    }

    pub(in crate::node_graph::fluid) fn record(&mut self, point: TakeTime) -> Result<(), String> {
        validate_point(point)?;

        if let Some(previous) = self.history.back().copied() {
            validate_step(previous, point)?;
            if previous == point {
                return Ok(());
            }
        }

        let sequence = self
            .observed_sequence
            .checked_add(1)
            .ok_or_else(|| "fluid take timing observation sequence overflowed".to_owned())?;
        let consumed_until = self
            .acknowledged_time
            .or_else(|| self.history.front().map(TakeTime::time))
            .unwrap_or(point.transport);
        self.history
            .record(point, consumed_until)
            .map_err(|error| format!("fluid take timing history: {error}"))?;
        self.observed_sequence = sequence;
        Ok(())
    }

    pub(in crate::node_graph::fluid) fn clear(&mut self) {
        self.history.clear();
        self.observed_sequence = 0;
        self.acknowledged_sequence = 0;
        self.acknowledged_time = None;
        // A missing spare belongs to a handoff still in flight. The reply will
        // restore it through `recycle`; otherwise retain its allocation.
        if let Some(spare) = &mut self.spare {
            spare.clear();
        }
    }

    pub(in crate::node_graph::fluid) fn pending(&self) -> bool {
        self.observed_sequence > self.acknowledged_sequence
    }

    pub(in crate::node_graph::fluid) fn snapshot(&mut self, metadata_only: bool) -> TimingHandoff {
        let mut points = self.spare.take().expect("one timing handoff in flight");
        points.clear();
        points.extend(self.history.iter().copied());
        TimingHandoff {
            sequence: self.observed_sequence,
            points,
            metadata_only,
        }
    }

    pub(in crate::node_graph::fluid) fn recycle(
        &mut self,
        handoff: TimingHandoff,
        accepted: bool,
    ) -> Result<(), String> {
        let sequence = handoff.sequence;

        if accepted {
            if sequence > self.observed_sequence {
                self.restore_spare(handoff.points);
                return Err(
                    "fluid take timing acknowledgement is ahead of observed sequence".into(),
                );
            }
            if sequence < self.acknowledged_sequence {
                self.restore_spare(handoff.points);
                return Err("fluid take timing acknowledgement regressed".into());
            }
            if sequence > self.acknowledged_sequence {
                let Some(last_point) = handoff.points.last().copied() else {
                    self.restore_spare(handoff.points);
                    return Err("fluid take timing acknowledgement has no last point".into());
                };
                let offset = sequence - self.acknowledged_sequence;
                let expected_index = if self.acknowledged_sequence == 0 {
                    offset - 1
                } else {
                    offset
                };
                if validate_point(last_point).is_err()
                    || self
                        .acknowledged_time
                        .is_some_and(|previous| last_point.transport.0 < previous.0)
                    || usize::try_from(expected_index)
                        .ok()
                        .and_then(|index| self.history.get(index))
                        != Some(&last_point)
                {
                    self.restore_spare(handoff.points);
                    return Err("fluid take timing acknowledgement has an invalid boundary".into());
                }
                let last_transport = last_point.transport;
                if let Err(error) = self.history.prune_before(last_transport) {
                    self.restore_spare(handoff.points);
                    return Err(format!("fluid take timing history: {error}"));
                }
                self.acknowledged_sequence = sequence;
                self.acknowledged_time = Some(last_transport);
            }
        }

        // Empty synthetic replies are common for legacy/live requests. Keep a
        // pre-existing spare in that case instead of replacing it with a zero
        // capacity vector.
        if handoff.points.is_empty() && self.spare.is_some() {
            return Ok(());
        }
        self.restore_spare(handoff.points);
        Ok(())
    }

    fn restore_spare(&mut self, points: Vec<TakeTime>) {
        self.spare = Some(points);
    }
}

pub(in crate::node_graph::fluid) fn validate_point(point: TakeTime) -> Result<(), String> {
    if !point.beat.0.is_finite()
        || !point.transport.0.is_finite()
        || !point.simulation.0.is_finite()
    {
        return Err("fluid take timing point must contain finite times".into());
    }
    if point.simulation.0 < 0.0 {
        return Err("fluid take timing simulation time must be nonnegative".into());
    }
    Ok(())
}

pub(in crate::node_graph::fluid) fn validate_step(
    previous: TakeTime,
    next: TakeTime,
) -> Result<(), String> {
    validate_point(previous)?;
    validate_point(next)?;
    if previous == next {
        return Ok(());
    }
    if next.transport.0 <= previous.transport.0 {
        return Err("fluid take timing transport must increase".into());
    }
    if next.beat.0 < previous.beat.0 {
        return Err("fluid take timing beats must not decrease".into());
    }
    if next.simulation.0 < previous.simulation.0 {
        return Err("fluid take timing simulation time must not decrease".into());
    }
    if next.beat == previous.beat && next.simulation != previous.simulation {
        return Err("fluid take timing equal beats require unchanged simulation".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(beat: f64, transport: f64, simulation: f64) -> TakeTime {
        TakeTime {
            beat: Beats(beat),
            transport: Seconds(transport),
            simulation: Seconds(simulation),
        }
    }

    #[test]
    fn fluid_take_clock_accepts_nonzero_and_negative_origins() {
        let mut capture = Capture::new(4);
        let first = point(-2.0, -1.0, 0.0);
        capture.record(first).unwrap();
        capture.record(point(-1.0, -0.5, 0.0)).unwrap();
        let handoff = capture.snapshot(false);
        assert_eq!(handoff.points, vec![first, point(-1.0, -0.5, 0.0)]);
        capture.recycle(handoff, true).unwrap();
        assert!(!capture.pending());
    }

    #[test]
    fn fluid_take_clock_accepts_variable_speed_and_long_held_spans() {
        let mut capture = Capture::new(5);
        capture.record(point(4.0, 10.0, 0.0)).unwrap();
        capture.record(point(4.0, 12.0, 0.0)).unwrap();
        capture.record(point(8.0, 13.0, 1_000_000.0)).unwrap();
        assert!(capture.pending());
    }

    #[test]
    fn fluid_take_clock_duplicate_points_are_inert() {
        let mut capture = Capture::new(3);
        let sample = point(2.0, 3.0, 1.0);
        capture.record(sample).unwrap();
        capture.record(sample).unwrap();
        let handoff = capture.snapshot(false);
        assert_eq!(handoff.sequence, 1);
        assert_eq!(handoff.points, vec![sample]);
        capture.recycle(handoff, true).unwrap();
        assert!(!capture.pending());
    }

    #[test]
    fn fluid_take_clock_grows_until_acknowledgement() {
        let mut capture = Capture::new(2);
        capture.record(point(0.0, 0.0, 0.0)).unwrap();
        capture.record(point(1.0, 1.0, 1.0)).unwrap();
        capture.record(point(2.0, 2.0, 2.0)).unwrap();
        capture.record(point(3.0, 3.0, 3.0)).unwrap();
        let handoff = capture.snapshot(false);
        assert_eq!(handoff.points.len(), 4);
        capture.recycle(handoff, true).unwrap();
        assert_eq!(capture.history.len(), 1);
    }

    #[test]
    fn fluid_take_clock_acknowledgement_prunes_only_accepted_boundary() {
        let mut capture = Capture::new(5);
        let first = point(0.0, 0.0, 0.0);
        let second = point(1.0, 1.0, 1.0);
        let third = point(2.0, 2.0, 2.0);
        capture.record(first).unwrap();
        capture.record(second).unwrap();
        let handoff = capture.snapshot(false);
        capture.record(third).unwrap();
        capture.recycle(handoff, true).unwrap();
        assert!(capture.pending());
        let next = capture.snapshot(false);
        assert_eq!(next.points, vec![second, third]);
        capture.recycle(next, true).unwrap();
        assert!(!capture.pending());
    }

    #[test]
    fn fluid_take_clock_failed_stale_reply_does_not_prune() {
        let mut capture = Capture::new(5);
        let first = point(0.0, 0.0, 0.0);
        let second = point(1.0, 1.0, 1.0);
        let third = point(2.0, 2.0, 2.0);
        capture.record(first).unwrap();
        capture.record(second).unwrap();
        let handoff = capture.snapshot(false);
        capture.record(third).unwrap();
        capture.recycle(handoff, false).unwrap();
        assert!(capture.pending());
        let retry = capture.snapshot(false);
        assert_eq!(retry.points, vec![first, second, third]);
        capture.recycle(retry, true).unwrap();
        assert!(!capture.pending());
    }

    #[test]
    fn fluid_take_clock_stale_reply_after_clear_recycles_storage() {
        let mut capture = Capture::new(4);
        capture.record(point(0.0, 0.0, 0.0)).unwrap();
        let handoff = capture.snapshot(false);
        capture.clear();
        assert!(capture.recycle(handoff, true).is_err());
        capture.record(point(0.0, 0.0, 0.0)).unwrap();
        let current = capture.snapshot(false);
        assert_eq!(current.points.len(), 1);
    }

    #[test]
    fn fluid_take_clock_reuses_storage_and_empty_replies_keep_spare() {
        let mut capture = Capture::new(4);
        capture.record(point(0.0, 0.0, 0.0)).unwrap();
        let handoff = capture.snapshot(false);
        let capacity = handoff.points.capacity();
        capture.recycle(handoff, true).unwrap();
        capture.recycle(TimingHandoff::default(), false).unwrap();
        capture.record(point(1.0, 1.0, 1.0)).unwrap();
        let next = capture.snapshot(false);
        assert!(next.points.capacity() >= capacity);
    }

    #[test]
    fn fluid_take_clock_rejects_invalid_and_nonmonotonic_points() {
        let mut capture = Capture::new(4);
        assert!(capture.record(point(0.0, 0.0, -1.0)).is_err());
        capture.record(point(1.0, 2.0, 1.0)).unwrap();
        assert!(capture.record(point(0.0, 3.0, 2.0)).is_err());
        assert!(capture.record(point(2.0, 2.0, 2.0)).is_err());
        assert!(capture.record(point(1.0, 3.0, 2.0)).is_err());
    }
}
