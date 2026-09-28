//! Bridge a backend's monotonic clock to `Instant` at the producer, once.
//! Callback arrival jitter must not retime subsequent source blocks. Absolute
//! accuracy is limited by the backend timestamp and the initial clock pairing.

use std::time::{Duration, Instant};

pub(super) struct CallbackClock<T> {
    anchor: Option<(T, Instant)>,
    last_callback: Option<T>,
    invalid: bool,
}

impl<T: Copy + Ord> CallbackClock<T> {
    pub(super) fn new() -> Self {
        Self {
            anchor: None,
            last_callback: None,
            invalid: false,
        }
    }

    pub(super) fn map(
        &mut self,
        callback: T,
        capture: T,
        arrival: Instant,
        duration_since: impl Fn(T, T) -> Option<Duration>,
    ) -> Option<Instant> {
        if self.invalid {
            return None;
        }
        if capture > callback || self.last_callback.is_some_and(|last| callback < last) {
            self.invalid = true;
            return None;
        }
        let (source, instant) = *self.anchor.get_or_insert((callback, arrival));
        let mapped = if capture >= source {
            duration_since(capture, source).and_then(|delta| instant.checked_add(delta))
        } else {
            duration_since(source, capture).and_then(|delta| instant.checked_sub(delta))
        };
        self.invalid = mapped.is_none();
        self.last_callback = Some(callback);
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elapsed(later: u64, earlier: u64) -> Option<Duration> {
        later.checked_sub(earlier).map(Duration::from_millis)
    }

    #[test]
    fn source_clock_keeps_latency_and_ignores_later_callback_jitter() {
        let now = Instant::now();
        let mut clock = CallbackClock::new();
        assert_eq!(
            clock.map(1000, 990, now, elapsed),
            now.checked_sub(Duration::from_millis(10))
        );
        assert_eq!(
            clock.map(1010, 1000, now + Duration::from_millis(80), elapsed),
            Some(now)
        );
        assert_eq!(
            clock.map(1020, 1010, now + Duration::from_secs(1), elapsed),
            Some(now + Duration::from_millis(10))
        );
    }

    #[test]
    fn invalid_source_clock_cannot_recover_with_a_fabricated_anchor() {
        let now = Instant::now();
        let mut clock = CallbackClock::new();
        assert!(clock.map(1000, 990, now, elapsed).is_some());
        assert_eq!(clock.map(900, 890, now, elapsed), None);
        assert_eq!(clock.map(1010, 1000, now, elapsed), None);
        let mut clock = CallbackClock::new();
        assert_eq!(clock.map(1000, 1001, now, elapsed), None);
        let mut clock = CallbackClock::new();
        assert_eq!(clock.map(1000, 990, now, |_, _| None), None);
    }
}
