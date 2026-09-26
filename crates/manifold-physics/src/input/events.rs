//! Fixed-tick delivery for already resolved discrete physics inputs.

use std::collections::VecDeque;
use std::fmt;

use crate::Seconds;
use crate::interaction::TickStamp;

const MAX_TICK: u64 = (1 << 53) - 1;
const MAX_CORRECTIONS: usize = 4;

/// A timestamped discrete input in one simulation epoch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EventStamp {
    pub epoch: u64,
    pub time: Seconds,
    pub sequence: u64,
}

/// An input delivered at the beginning of a native simulation tick.
#[derive(Debug, PartialEq)]
pub struct AppliedEvent<T> {
    pub source: EventStamp,
    pub applied: TickStamp,
    pub lateness: Seconds,
    pub value: T,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventError {
    InvalidCapacity,
    InvalidEpoch,
    InvalidClock,
    InvalidTime,
    EpochMismatch,
    StaleReset,
    NonIncreasingSequence,
    OutOfOrderTick,
    CapacityOverflow,
    UnrepresentableTickRange,
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCapacity => "event queue capacity must be positive",
            Self::InvalidEpoch => "event queue epoch must be non-zero",
            Self::InvalidClock => "event queue clock must be finite with a positive tick duration",
            Self::InvalidTime => "event timestamp must be finite",
            Self::EpochMismatch => "event belongs to a different simulation epoch",
            Self::StaleReset => "event queue reset epoch must be strictly newer",
            Self::NonIncreasingSequence => {
                "event producer sequence must strictly increase within an epoch"
            }
            Self::OutOfOrderTick => "event queue tick must be the next unstarted tick",
            Self::CapacityOverflow => "event queue capacity overflow is latched until reset",
            Self::UnrepresentableTickRange => {
                "event timestamp falls outside the representable tick range"
            }
        })
    }
}

impl std::error::Error for EventError {}

struct QueuedEvent<T> {
    target: u64,
    source: EventStamp,
    value: T,
}

/// A fixed-capacity, tick-aligned queue of discrete inputs. Times share the
/// constructor's origin and epoch. The producer assigns increasing sequences
/// in arrival order; event timestamps may arrive out of order.
///
/// Overflow preserves unread events and stops delivery until a new epoch.
/// Construction allocates the storage; enqueue, tick delivery and reset reuse it.
pub struct EventQueue<T> {
    epoch: u64,
    origin: Seconds,
    tick_duration: Seconds,
    next_tick: u64,
    last_sequence: Option<u64>,
    events: VecDeque<QueuedEvent<T>>,
    capacity: usize,
    exhausted: bool,
}

impl<T> EventQueue<T> {
    pub fn new(
        epoch: u64,
        origin: Seconds,
        tick_duration: Seconds,
        capacity: usize,
    ) -> Result<Self, EventError> {
        if capacity == 0 {
            return Err(EventError::InvalidCapacity);
        }
        if epoch == 0 {
            return Err(EventError::InvalidEpoch);
        }
        if !origin.0.is_finite() || !tick_duration.0.is_finite() || tick_duration.0 <= 0.0 {
            return Err(EventError::InvalidClock);
        }
        tick_boundaries(origin, tick_duration, 0)?;
        Ok(Self {
            epoch,
            origin,
            tick_duration,
            next_tick: 0,
            last_sequence: None,
            events: VecDeque::with_capacity(capacity),
            capacity,
            exhausted: false,
        })
    }

    /// Add an event and return the tick at which it is planned to be applied.
    /// The returned tick is not evidence that the native tick has completed.
    pub fn enqueue(&mut self, stamp: EventStamp, value: T) -> Result<TickStamp, EventError> {
        if self.exhausted {
            return Err(EventError::CapacityOverflow);
        }
        self.validate_stamp(stamp)?;
        let nominal = self.nominal_tick(stamp.time)?;
        if self.next_tick > MAX_TICK {
            return Err(EventError::UnrepresentableTickRange);
        }
        let target = nominal.max(self.next_tick);
        let (start, _) = self.boundaries(target)?;
        let lateness = (start - stamp.time.0).max(0.0);
        if !lateness.is_finite() {
            return Err(EventError::UnrepresentableTickRange);
        }
        if self.events.len() == self.capacity {
            self.exhausted = true;
            return Err(EventError::CapacityOverflow);
        }

        let queued = QueuedEvent {
            target,
            source: stamp,
            value,
        };
        let position = self.events.iter().position(|existing| {
            (
                existing.target,
                existing.source.time.0,
                existing.source.sequence,
            ) > (target, stamp.time.0, stamp.sequence)
        });
        match position {
            Some(index) => self.events.insert(index, queued),
            None => self.events.push_back(queued),
        }
        self.last_sequence = Some(stamp.sequence);
        Ok(TickStamp {
            epoch: self.epoch,
            tick: target,
        })
    }

    /// Start the next consecutive tick and deliver each assigned event once.
    /// The owner applies these inputs before stepping its native world; a
    /// callback receipt does not prove that the native step completed. Empty
    /// ticks also advance the cursor, so later input cannot rewrite them.
    pub fn begin_tick(
        &mut self,
        tick: TickStamp,
        mut consume: impl FnMut(AppliedEvent<T>),
    ) -> Result<(), EventError> {
        if self.exhausted {
            return Err(EventError::CapacityOverflow);
        }
        if tick.epoch != self.epoch {
            return Err(EventError::EpochMismatch);
        }
        if tick.tick != self.next_tick {
            return Err(EventError::OutOfOrderTick);
        }
        let (start, _) = self.boundaries(self.next_tick)?;
        let following = self
            .next_tick
            .checked_add(1)
            .ok_or(EventError::UnrepresentableTickRange)?;
        self.boundaries(following)?;
        let applied = tick;
        self.next_tick = following;

        while self
            .events
            .front()
            .is_some_and(|event| event.target == applied.tick)
        {
            let event = self.events.pop_front().expect("front checked above");
            let lateness = (start - event.source.time.0).max(0.0);
            consume(AppliedEvent {
                source: event.source,
                applied,
                lateness: Seconds(lateness),
                value: event.value,
            });
        }
        Ok(())
    }

    /// Start a newer epoch and return the number of cancelled unread events.
    /// Validation precedes mutation; storage and tick duration are retained.
    pub fn reset(&mut self, new_epoch: u64, new_origin: Seconds) -> Result<usize, EventError> {
        if new_epoch == 0 {
            return Err(EventError::InvalidEpoch);
        }
        if new_epoch <= self.epoch {
            return Err(EventError::StaleReset);
        }
        if !new_origin.0.is_finite() {
            return Err(EventError::InvalidClock);
        }
        tick_boundaries(new_origin, self.tick_duration, 0)?;

        let cancelled = self.events.len();
        self.epoch = new_epoch;
        self.origin = new_origin;
        self.next_tick = 0;
        self.last_sequence = None;
        self.events.clear();
        self.exhausted = false;
        Ok(cancelled)
    }

    pub fn next_tick(&self) -> TickStamp {
        TickStamp {
            epoch: self.epoch,
            tick: self.next_tick,
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    fn validate_stamp(&self, stamp: EventStamp) -> Result<(), EventError> {
        if stamp.epoch != self.epoch {
            return Err(EventError::EpochMismatch);
        }
        if !stamp.time.0.is_finite() {
            return Err(EventError::InvalidTime);
        }
        if self
            .last_sequence
            .is_some_and(|last| stamp.sequence <= last)
        {
            return Err(EventError::NonIncreasingSequence);
        }
        Ok(())
    }

    fn nominal_tick(&self, time: Seconds) -> Result<u64, EventError> {
        if time.0 <= self.origin.0 {
            return Ok(0);
        }
        let quotient = (time.0 - self.origin.0) / self.tick_duration.0;
        if !quotient.is_finite() || quotient < 0.0 || quotient >= (MAX_TICK + 1) as f64 {
            return Err(EventError::UnrepresentableTickRange);
        }
        let mut candidate = quotient.floor() as u64;
        for _ in 0..MAX_CORRECTIONS {
            let (lower, upper) = self.boundaries(candidate)?;
            if time.0 < lower {
                if candidate == 0 {
                    return Ok(0);
                }
                candidate -= 1;
            } else if time.0 >= upper {
                candidate = candidate
                    .checked_add(1)
                    .ok_or(EventError::UnrepresentableTickRange)?;
                if candidate > MAX_TICK {
                    return Err(EventError::UnrepresentableTickRange);
                }
            } else {
                return Ok(candidate);
            }
        }
        Err(EventError::UnrepresentableTickRange)
    }

    fn boundaries(&self, tick: u64) -> Result<(f64, f64), EventError> {
        tick_boundaries(self.origin, self.tick_duration, tick)
    }
}

fn tick_boundaries(
    origin: Seconds,
    duration: Seconds,
    tick: u64,
) -> Result<(f64, f64), EventError> {
    if tick > MAX_TICK {
        return Err(EventError::UnrepresentableTickRange);
    }
    let lower = origin.0 + tick as f64 * duration.0;
    let upper = origin.0 + (tick + 1) as f64 * duration.0;
    if !lower.is_finite() || !upper.is_finite() || lower >= upper {
        return Err(EventError::UnrepresentableTickRange);
    }
    Ok((lower, upper))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(epoch: u64, time: f64, sequence: u64) -> EventStamp {
        EventStamp {
            epoch,
            time: Seconds(time),
            sequence,
        }
    }

    #[test]
    fn equal_times_are_stable_and_source_times_are_sorted() {
        let mut queue = EventQueue::new(1, Seconds(10.0), Seconds(1.0 / 60.0), 8).unwrap();
        queue.enqueue(stamp(1, 10.01, 0), 'a').unwrap();
        queue.enqueue(stamp(1, 10.0, 1), 'b').unwrap();
        queue.enqueue(stamp(1, 10.0, 2), 'c').unwrap();
        let mut values = Vec::new();
        queue
            .begin_tick(queue.next_tick(), |event| values.push(event.value))
            .unwrap();
        assert_eq!(values, vec!['b', 'c', 'a']);
    }

    #[test]
    fn exact_boundary_enters_the_next_tick() {
        let mut queue = EventQueue::new(1, Seconds(0.0), Seconds(1.0 / 60.0), 4).unwrap();
        let planned = queue.enqueue(stamp(1, 1.0 / 60.0, 0), ()).unwrap();
        assert_eq!(planned.tick, 1);
    }

    #[test]
    fn adjacent_samples_at_nonzero_origin_do_not_cross_a_boundary() {
        let origin: f64 = 10_000.25;
        let duration: f64 = 1.0 / 60.0;
        let boundary = origin + duration;
        let below = f64::from_bits(boundary.to_bits() - 1);
        let mut queue = EventQueue::new(1, Seconds(origin), Seconds(duration), 4).unwrap();
        assert_eq!(queue.enqueue(stamp(1, below, 0), ()).unwrap().tick, 0);
        assert_eq!(queue.enqueue(stamp(1, boundary, 1), ()).unwrap().tick, 1);
    }

    #[test]
    fn late_event_targets_next_tick_and_records_delay() {
        let mut queue = EventQueue::new(1, Seconds(0.0), Seconds(1.0), 4).unwrap();
        queue.begin_tick(queue.next_tick(), |_| {}).unwrap();
        queue.enqueue(stamp(1, 0.25, 0), 'x').unwrap();
        let mut lateness = None;
        queue
            .begin_tick(queue.next_tick(), |event| lateness = Some(event.lateness.0))
            .unwrap();
        assert_eq!(lateness, Some(0.75));
    }

    #[test]
    fn overflow_is_sticky_and_reset_cancels_unread_events() {
        let mut queue = EventQueue::new(1, Seconds(0.0), Seconds(1.0), 1).unwrap();
        queue.enqueue(stamp(1, 0.0, 0), 'a').unwrap();
        assert_eq!(
            queue.enqueue(stamp(1, 0.0, 1), 'b'),
            Err(EventError::CapacityOverflow)
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(
            queue.begin_tick(queue.next_tick(), |_| {}),
            Err(EventError::CapacityOverflow)
        );
        assert_eq!(queue.reset(2, Seconds(0.0)), Ok(1));
        queue.enqueue(stamp(2, 0.0, 0), 'c').unwrap();
    }

    #[test]
    fn malformed_and_out_of_order_inputs_do_not_change_cursor_or_prefix() {
        assert!(matches!(
            EventQueue::<()>::new(1, Seconds(0.0), Seconds(0.0), 2),
            Err(EventError::InvalidClock)
        ));
        assert!(matches!(
            EventQueue::<()>::new(1, Seconds(1.0), Seconds(f64::EPSILON / 2.0), 2),
            Err(EventError::UnrepresentableTickRange)
        ));

        let mut queue = EventQueue::new(1, Seconds(0.0), Seconds(1.0), 2).unwrap();
        queue.enqueue(stamp(1, 0.0, 7), ()).unwrap();
        assert_eq!(queue.enqueue(stamp(1, -0.5, 8), ()).unwrap().tick, 0);
        assert_eq!(
            queue.enqueue(stamp(1, f64::NAN, 9), ()),
            Err(EventError::InvalidTime)
        );
        assert_eq!(
            queue.enqueue(stamp(1, 0.0, 7), ()),
            Err(EventError::NonIncreasingSequence)
        );
        assert_eq!(
            queue.begin_tick(TickStamp { epoch: 1, tick: 1 }, |_| {}),
            Err(EventError::OutOfOrderTick)
        );
        assert_eq!(queue.next_tick().tick, 0);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn reset_allows_sequence_restart_and_preserves_capacity() {
        let mut queue = EventQueue::new(1, Seconds(0.0), Seconds(1.0), 2).unwrap();
        let capacity = queue.events.capacity();
        queue.enqueue(stamp(1, 0.0, 10), ()).unwrap();
        assert_eq!(queue.reset(2, Seconds(4.0)).unwrap(), 1);
        assert_eq!(queue.events.capacity(), capacity);
        queue.enqueue(stamp(2, 4.0, 0), ()).unwrap();
        queue.begin_tick(queue.next_tick(), |_| {}).unwrap();
        for cycle in 1..10 {
            queue
                .enqueue(stamp(2, 4.0 + cycle as f64, cycle), ())
                .unwrap();
            queue.begin_tick(queue.next_tick(), |_| {}).unwrap();
        }
        assert_eq!(queue.events.capacity(), capacity);
    }

    #[test]
    fn half_open_boundaries_hold_at_both_adjacent_floats_over_long_timelines() {
        for origin in [-120.5, 0.0, 17.25, 10_000.25] {
            for duration in [1.0 / 60.0, 0.1] {
                let mut queue = EventQueue::new(1, Seconds(origin), Seconds(duration), 64).unwrap();
                let mut sequence = 0;
                for tick in [1, 2, 3, 10, 60, 6_000, 5_184_000] {
                    let boundary = origin + tick as f64 * duration;
                    for (time, expected) in [
                        (boundary.next_down(), tick - 1),
                        (boundary, tick),
                        (boundary.next_up(), tick),
                    ] {
                        assert_eq!(
                            queue.enqueue(stamp(1, time, sequence), ()).unwrap().tick,
                            expected,
                            "origin {origin}, duration {duration}, boundary {boundary}, time {time}"
                        );
                        sequence += 1;
                    }
                }
            }
        }
    }

    #[test]
    fn stale_epochs_invalid_resets_and_duplicate_ticks_leave_events_intact() {
        let mut queue = EventQueue::new(2, Seconds(4.0), Seconds(1.0), 4).unwrap();
        queue.enqueue(stamp(2, 4.5, 8), 'a').unwrap();
        for epoch in [1, 3] {
            assert_eq!(
                queue.enqueue(stamp(epoch, 4.0, 9), 'b'),
                Err(EventError::EpochMismatch)
            );
            assert_eq!(
                queue.begin_tick(TickStamp { epoch, tick: 0 }, |_| panic!("wrong epoch")),
                Err(EventError::EpochMismatch)
            );
        }
        assert_eq!(queue.reset(2, Seconds(0.0)), Err(EventError::StaleReset));
        assert_eq!(
            queue.reset(3, Seconds(f64::INFINITY)),
            Err(EventError::InvalidClock)
        );
        assert_eq!(
            queue.reset(3, Seconds(f64::MAX)),
            Err(EventError::UnrepresentableTickRange)
        );
        assert_eq!(queue.len(), 1);
        let mut applied = Vec::new();
        queue
            .begin_tick(queue.next_tick(), |event| applied.push(event.value))
            .unwrap();
        assert_eq!(applied, ['a']);
        assert_eq!(
            queue.begin_tick(TickStamp { epoch: 2, tick: 0 }, |_| panic!("replayed tick")),
            Err(EventError::OutOfOrderTick)
        );
        assert_eq!(queue.next_tick(), TickStamp { epoch: 2, tick: 1 });
        assert_eq!(queue.reset(3, Seconds(0.0)).unwrap(), 0);
        assert_eq!(
            queue.enqueue(stamp(2, 0.0, 10), 'x'),
            Err(EventError::EpochMismatch)
        );
        queue.enqueue(stamp(3, 0.0, 0), 'c').unwrap();
        queue
            .begin_tick(queue.next_tick(), |event| applied.push(event.value))
            .unwrap();
        assert_eq!(applied, ['a', 'c']);
    }

    #[test]
    fn invalid_configuration_and_derived_overflow_are_rejected_before_mutation() {
        assert!(matches!(
            EventQueue::<()>::new(1, Seconds::ZERO, Seconds(1.0), 0),
            Err(EventError::InvalidCapacity)
        ));
        assert!(matches!(
            EventQueue::<()>::new(0, Seconds::ZERO, Seconds(1.0), 1),
            Err(EventError::InvalidEpoch)
        ));
        for duration in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                EventQueue::<()>::new(1, Seconds::ZERO, Seconds(duration), 1),
                Err(EventError::InvalidClock)
            ));
        }
        let mut queue =
            EventQueue::new(1, Seconds(f64::MAX * 0.75), Seconds(f64::MAX * 0.125), 1).unwrap();
        assert_eq!(
            queue.enqueue(stamp(1, -f64::MAX, 0), ()),
            Err(EventError::UnrepresentableTickRange)
        );
        assert!(queue.is_empty());
        assert!(!queue.is_exhausted());
        assert_eq!(queue.next_tick().tick, 0);

        let mut queue = EventQueue::new(1, Seconds::ZERO, Seconds(1.0 / 60.0), 2).unwrap();
        for time in [f64::NAN, f64::NEG_INFINITY, f64::INFINITY] {
            assert_eq!(
                queue.enqueue(stamp(1, time, 0), ()),
                Err(EventError::InvalidTime)
            );
        }
        assert_eq!(
            queue.enqueue(stamp(1, f64::MAX, 0), ()),
            Err(EventError::UnrepresentableTickRange)
        );
        queue.enqueue(stamp(1, 0.0, 0), ()).unwrap();
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn unrepresentable_next_tick_does_not_consume_the_current_prefix() {
        let origin = (1_u64 << 53) as f64;
        let mut queue = EventQueue::new(1, Seconds(origin), Seconds(1.5), 2).unwrap();
        queue.begin_tick(queue.next_tick(), |_| {}).unwrap();
        queue.enqueue(stamp(1, origin + 2.0, 0), 'a').unwrap();
        assert_eq!(
            queue.begin_tick(queue.next_tick(), |_| panic!("cannot start this tick")),
            Err(EventError::UnrepresentableTickRange)
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.next_tick().tick, 1);
    }
}
