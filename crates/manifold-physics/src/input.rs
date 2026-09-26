//! Retained authored inputs shared by rigid and fluid simulation adapters.
//!
//! Times belong to one simulation epoch. Owners clear history when resetting
//! that epoch and prune only after the consuming native ticks have completed.
//! Discrete event ordering is separate from this continuous-control history.

use std::collections::VecDeque;
use std::fmt;

use crate::Seconds;

pub trait Timestamped {
    fn time(&self) -> Seconds;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    InvalidCapacity,
    InvalidTime,
    NonMonotonic,
    CapacityExceeded,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCapacity => "physics input history requires at least two slots",
            Self::InvalidTime => "physics input history time must be finite",
            Self::NonMonotonic => "physics input history cannot move backward within an epoch",
            Self::CapacityExceeded => {
                "physics input history is full; restart the simulation or bake the scene"
            }
        })
    }
}

impl std::error::Error for HistoryError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryWrite {
    Appended,
    Replaced,
}

/// Fixed-capacity history that never overwrites unread inputs to make room.
/// Overflow latches until `clear`, so a caller cannot continue across a lost
/// sample merely because a worker later frees some storage.
pub struct InputHistory<T> {
    samples: VecDeque<T>,
    limit: usize,
    exhausted: bool,
}

impl<T: Timestamped> InputHistory<T> {
    pub fn with_capacity(capacity: usize) -> Result<Self, HistoryError> {
        if capacity < 2 {
            return Err(HistoryError::InvalidCapacity);
        }
        Ok(Self {
            samples: VecDeque::with_capacity(capacity),
            limit: capacity,
            exhausted: false,
        })
    }

    /// Record a control sample without changing an unfinished interpolation
    /// interval. A changed value at an existing, unconsumed endpoint keeps the
    /// old endpoint followed by the edit at the same time. Further edits replace
    /// only that second entry. At the exact timestamp sampling selects the edit;
    /// earlier queries still interpolate toward the original endpoint.
    ///
    /// `consumed_until` is the latest completed simulation time. Callers may
    /// avoid recording identical values at identical times when all associated
    /// inputs (including parallel role storage) are known to be unchanged.
    pub fn record(
        &mut self,
        sample: T,
        consumed_until: Seconds,
    ) -> Result<HistoryWrite, HistoryError> {
        if self.exhausted {
            return Err(HistoryError::CapacityExceeded);
        }
        let time = sample.time().0;
        if !time.is_finite() || !consumed_until.0.is_finite() {
            return Err(HistoryError::InvalidTime);
        }
        if let Some(last) = self.samples.back() {
            let previous = last.time().0;
            if time < previous {
                return Err(HistoryError::NonMonotonic);
            }
            if time == previous {
                let preserve_endpoint = time > consumed_until.0
                    && self.samples.len() >= 2
                    && self.samples[self.samples.len() - 2].time().0 < time;
                if !preserve_endpoint {
                    *self.samples.back_mut().expect("checked above") = sample;
                    return Ok(HistoryWrite::Replaced);
                }
            }
        }
        if self.samples.len() == self.limit {
            self.exhausted = true;
            return Err(HistoryError::CapacityExceeded);
        }
        self.samples.push_back(sample);
        Ok(HistoryWrite::Appended)
    }

    /// Drop inputs older than the lower bracket needed at `retain_from`.
    /// Returns the number removed for adapters with parallel packed storage.
    pub fn prune_before(&mut self, retain_from: Seconds) -> Result<usize, HistoryError> {
        if self.exhausted {
            return Err(HistoryError::CapacityExceeded);
        }
        if !retain_from.0.is_finite() {
            return Err(HistoryError::InvalidTime);
        }
        let mut removed = 0;
        while self.samples.len() > 1 && self.samples[1].time().0 <= retain_from.0 {
            self.samples.pop_front();
            removed += 1;
        }
        Ok(removed)
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.exhausted = false;
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn front(&self) -> Option<&T> {
        self.samples.front()
    }

    pub fn back(&self) -> Option<&T> {
        self.samples.back()
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        self.samples.get(index)
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &T> + ExactSizeIterator {
        self.samples.iter()
    }
}

/// A continuous interpolation span. Indices refer to the supplied sequence and
/// also address parallel packed inputs. Switches use `before` until alpha=1.
pub struct InputSpan<'a, T> {
    pub before: &'a T,
    pub after: &'a T,
    pub before_index: usize,
    pub after_index: usize,
    pub alpha: f32,
}

/// Select a span from chronological history or an immutable worker snapshot.
/// Equal-time edits are right-continuous; outside the range, hold the nearest
/// sample. Invalid query times and empty input return `None`.
pub fn input_span<'a, T: Timestamped + 'a>(
    history: impl IntoIterator<Item = &'a T>,
    time: Seconds,
) -> Option<InputSpan<'a, T>> {
    span_at(history, time, false)
}

/// Sample the left side of a discontinuity, for an animated target closing a
/// simulation interval. An edit at the interval's upper boundary must not sweep
/// that new pose through the interval which precedes the edit.
pub fn input_span_before<'a, T: Timestamped + 'a>(
    history: impl IntoIterator<Item = &'a T>,
    time: Seconds,
) -> Option<InputSpan<'a, T>> {
    span_at(history, time, true)
}

fn span_at<'a, T: Timestamped + 'a>(
    history: impl IntoIterator<Item = &'a T>,
    time: Seconds,
    before_boundary: bool,
) -> Option<InputSpan<'a, T>> {
    if !time.0.is_finite() {
        return None;
    }
    let mut history = history.into_iter().enumerate();
    let (mut before_index, mut before) = history.next()?;
    if time.0 > before.time().0 || (!before_boundary && time.0 == before.time().0) {
        for (after_index, after) in history {
            if after.time().0 > time.0 || (before_boundary && after.time().0 == time.0) {
                let delta = after.time().0 - before.time().0;
                let alpha = if delta.is_finite() {
                    (time.0 - before.time().0) / delta
                } else {
                    (time.0 * 0.5 - before.time().0 * 0.5)
                        / (after.time().0 * 0.5 - before.time().0 * 0.5)
                };
                return Some(InputSpan {
                    before,
                    after,
                    before_index,
                    after_index,
                    alpha: alpha.clamp(0.0, 1.0) as f32,
                });
            }
            before = after;
            before_index = after_index;
        }
    }
    Some(InputSpan {
        before,
        after: before,
        before_index,
        after_index: before_index,
        alpha: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Sample(f64, f32);

    impl Timestamped for Sample {
        fn time(&self) -> Seconds {
            Seconds(self.0)
        }
    }

    fn value(history: &InputHistory<Sample>, time: f64) -> f32 {
        let span = input_span(history.iter(), Seconds(time)).unwrap();
        span.before.1 + span.alpha * (span.after.1 - span.before.1)
    }

    #[test]
    fn unfinished_ramp_survives_same_time_edits() {
        let mut history = InputHistory::with_capacity(4).unwrap();
        history.record(Sample(0.0, 0.0), Seconds(0.0)).unwrap();
        history.record(Sample(1.0, 10.0), Seconds(0.0)).unwrap();
        assert_eq!(
            history.record(Sample(1.0, 20.0), Seconds(0.0)),
            Ok(HistoryWrite::Appended)
        );
        assert_eq!(
            history.record(Sample(1.0, 30.0), Seconds(0.0)),
            Ok(HistoryWrite::Replaced)
        );
        assert_eq!(history.len(), 3);
        assert_eq!(value(&history, 0.5), 5.0);
        assert_eq!(value(&history, 1.0), 30.0);
        assert_eq!(value(&history, 1.1), 30.0);
        history.record(Sample(2.0, 40.0), Seconds(0.0)).unwrap();
        assert_eq!(value(&history, 1.5), 35.0);
        let span = input_span(history.iter(), Seconds(1.0)).unwrap();
        assert_eq!((span.before_index, span.after_index), (2, 3));
        let closing = input_span_before(history.iter(), Seconds(1.0)).unwrap();
        assert_eq!((closing.before_index, closing.after_index), (0, 1));
        assert_eq!(closing.alpha, 1.0);
        assert_eq!(closing.after.1, 10.0);
        let after = input_span_before(history.iter(), Seconds(1.5)).unwrap();
        assert_eq!((after.before_index, after.after_index), (2, 3));
    }

    #[test]
    fn consumed_or_initial_endpoint_replaces_without_growing() {
        let mut history = InputHistory::with_capacity(3).unwrap();
        history.record(Sample(0.0, 0.0), Seconds(0.0)).unwrap();
        assert_eq!(
            history.record(Sample(0.0, 5.0), Seconds(0.0)),
            Ok(HistoryWrite::Replaced)
        );
        history.record(Sample(1.0, 10.0), Seconds(1.0)).unwrap();
        assert_eq!(
            history.record(Sample(1.0, 20.0), Seconds(1.0)),
            Ok(HistoryWrite::Replaced)
        );
        assert_eq!(history.len(), 2);
        assert_eq!(value(&history, 1.0), 20.0);
    }

    #[test]
    fn overflow_preserves_prefix_and_latches_until_clear() {
        let mut history = InputHistory::with_capacity(2).unwrap();
        let capacity = history.samples.capacity();
        history.record(Sample(0.0, 0.0), Seconds(0.0)).unwrap();
        history.record(Sample(1.0, 1.0), Seconds(0.0)).unwrap();
        // Preserving an unread endpoint requires a slot, even at the same time.
        assert_eq!(
            history.record(Sample(1.0, 2.0), Seconds(0.0)),
            Err(HistoryError::CapacityExceeded)
        );
        assert!(history.is_exhausted());
        assert_eq!(
            history.prune_before(Seconds(2.0)),
            Err(HistoryError::CapacityExceeded)
        );
        assert_eq!(
            history.record(Sample(1.0, 3.0), Seconds(2.0)),
            Err(HistoryError::CapacityExceeded)
        );
        assert_eq!(
            history.iter().copied().collect::<Vec<_>>(),
            vec![Sample(0.0, 0.0), Sample(1.0, 1.0)]
        );
        assert_eq!(history.samples.capacity(), capacity);
        history.clear();
        assert!(!history.is_exhausted());
        history.record(Sample(-1.0, 4.0), Seconds(-1.0)).unwrap();
        assert_eq!(value(&history, -1.0), 4.0);
        assert_eq!(history.samples.capacity(), capacity);
    }

    #[test]
    fn prune_keeps_required_bracket_and_matching_indices() {
        let mut history = InputHistory::with_capacity(4).unwrap();
        for sample in [
            Sample(0.0, 0.0),
            Sample(1.0, 10.0),
            Sample(1.0, 20.0),
            Sample(2.0, 30.0),
        ] {
            history.record(sample, Seconds(0.0)).unwrap();
        }
        assert_eq!(history.prune_before(Seconds(0.5)), Ok(0));
        assert_eq!(history.prune_before(Seconds(1.0)), Ok(2));
        assert_eq!(value(&history, 1.5), 25.0);
        let span = input_span(history.iter(), Seconds(1.5)).unwrap();
        assert_eq!((span.before_index, span.after_index), (0, 1));
        assert_eq!(history.prune_before(Seconds(3.0)), Ok(1));
        assert_eq!(value(&history, 3.0), 30.0);
    }

    #[test]
    fn malformed_input_is_atomic_and_does_not_poison_valid_history() {
        assert!(matches!(
            InputHistory::<Sample>::with_capacity(1),
            Err(HistoryError::InvalidCapacity)
        ));
        let mut history = InputHistory::with_capacity(3).unwrap();
        history.record(Sample(1.0, 2.0), Seconds(0.0)).unwrap();
        assert_eq!(
            history.record(Sample(f64::NAN, 3.0), Seconds(0.0)),
            Err(HistoryError::InvalidTime)
        );
        assert_eq!(
            history.record(Sample(2.0, 3.0), Seconds(f64::INFINITY)),
            Err(HistoryError::InvalidTime)
        );
        assert_eq!(
            history.record(Sample(0.0, 3.0), Seconds(0.0)),
            Err(HistoryError::NonMonotonic)
        );
        assert_eq!(history.len(), 1);
        assert!(!history.is_exhausted());
        history.record(Sample(2.0, 4.0), Seconds(0.0)).unwrap();
        assert_eq!(value(&history, 1.5), 3.0);
        assert!(input_span(history.iter(), Seconds(f64::NAN)).is_none());
        history.clear();
        assert!(input_span(history.iter(), Seconds(0.0)).is_none());
    }

    #[test]
    fn extreme_finite_span_and_out_of_range_hold_are_defined() {
        let mut history = InputHistory::with_capacity(2).unwrap();
        history
            .record(Sample(-1e308, 0.0), Seconds(-1e308))
            .unwrap();
        history.record(Sample(1e308, 1.0), Seconds(-1e308)).unwrap();
        assert_eq!(value(&history, 0.0), 0.5);
        assert_eq!(value(&history, -f64::MAX), 0.0);
        assert_eq!(value(&history, f64::MAX), 1.0);
    }
}
