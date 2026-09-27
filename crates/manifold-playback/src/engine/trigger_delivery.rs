use manifold_core::{Beats, Seconds};

use crate::modulation::TriggerPulse;

/// Maximum number of trigger pulses retained between renderer consumers.
pub const DEFAULT_TRIGGER_DELIVERY_CAPACITY: usize = 4096;

/// A trigger pulse together with the engine clock at which it was accepted.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedTriggerPulse {
    pub pulse: TriggerPulse,
    pub epoch: u64,
    pub sequence: u64,
    pub accepted_time: Seconds,
    pub accepted_beat: Beats,
}

/// The reason trigger delivery stopped accepting new pulses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerDeliveryError {
    CapacityOverflow,
    InvalidClock,
    SequenceExhausted,
    EpochExhausted,
}

impl std::fmt::Display for TriggerDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::CapacityOverflow => "too many pending events",
            Self::InvalidClock => "engine clock is invalid",
            Self::SequenceExhausted => "input sequence is exhausted",
            Self::EpochExhausted => "input epoch is exhausted",
        };
        f.write_str(message)
    }
}

impl std::error::Error for TriggerDeliveryError {}

/// A latched trigger-delivery failure. The epoch identifies the queue state in
/// which the failure occurred, even after transport state has moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerDeliveryFailure {
    pub epoch: u64,
    pub kind: TriggerDeliveryError,
}

impl std::fmt::Display for TriggerDeliveryFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self.kind {
            TriggerDeliveryError::CapacityOverflow => {
                "Trigger delivery stopped: too many pending events. Stop playback or seek to restart."
            }
            TriggerDeliveryError::InvalidClock => {
                "Trigger delivery stopped: engine clock is invalid. Stop playback or seek to restart."
            }
            TriggerDeliveryError::SequenceExhausted => {
                "Trigger delivery stopped: sequence space is exhausted. Stop playback or seek to restart."
            }
            TriggerDeliveryError::EpochExhausted => {
                "Trigger delivery stopped: epoch space is exhausted. Restart the app to recover."
            }
        };
        f.write_str(message)
    }
}

impl std::error::Error for TriggerDeliveryFailure {}

#[derive(Debug)]
pub(crate) struct TriggerDeliveryQueue {
    pulses: Vec<CapturedTriggerPulse>,
    capacity: usize,
    epoch: u64,
    next_sequence: u64,
    failure: Option<TriggerDeliveryFailure>,
}

impl TriggerDeliveryQueue {
    pub(crate) fn new() -> Self {
        Self::with_capacity(DEFAULT_TRIGGER_DELIVERY_CAPACITY)
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            pulses: Vec::with_capacity(capacity),
            capacity,
            epoch: 0,
            next_sequence: 0,
            failure: None,
        }
    }

    pub(crate) fn reset(&mut self) -> Result<(), TriggerDeliveryError> {
        let Some(epoch) = self.epoch.checked_add(1) else {
            let error = TriggerDeliveryError::EpochExhausted;
            self.failure = Some(TriggerDeliveryFailure {
                epoch: self.epoch,
                kind: error,
            });
            return Err(error);
        };
        self.epoch = epoch;
        self.next_sequence = 0;
        self.pulses.clear();
        self.failure = None;
        Ok(())
    }

    pub(crate) fn append_batch(
        &mut self,
        pulses: &mut Vec<TriggerPulse>,
        accepted_time: Seconds,
        accepted_beat: Beats,
    ) -> Result<(), TriggerDeliveryError> {
        if let Some(failure) = self.failure {
            return Err(failure.kind);
        }
        if pulses.is_empty() {
            return Ok(());
        }
        if !accepted_time.0.is_finite() || !accepted_beat.0.is_finite() {
            return Err(self.latch(TriggerDeliveryError::InvalidClock));
        }
        if pulses.len() > self.capacity.saturating_sub(self.pulses.len()) {
            return Err(self.latch(TriggerDeliveryError::CapacityOverflow));
        }
        let Some(end_sequence) = self.next_sequence.checked_add(pulses.len() as u64) else {
            return Err(self.latch(TriggerDeliveryError::SequenceExhausted));
        };

        // All validation is complete before the first drain, so a failed batch
        // never exposes a partial prefix. The source Vec is reused as the
        // modulation scratch after its pulses move into the retained queue.
        for (offset, pulse) in pulses.drain(..).enumerate() {
            self.pulses.push(CapturedTriggerPulse {
                pulse,
                epoch: self.epoch,
                sequence: self.next_sequence + offset as u64,
                accepted_time,
                accepted_beat,
            });
        }
        self.next_sequence = end_sequence;
        Ok(())
    }

    fn latch(&mut self, kind: TriggerDeliveryError) -> TriggerDeliveryError {
        self.failure = Some(TriggerDeliveryFailure {
            epoch: self.epoch,
            kind,
        });
        kind
    }

    pub(crate) fn failure(&self) -> Option<TriggerDeliveryFailure> {
        self.failure
    }

    pub(crate) fn as_slice(&self) -> &[CapturedTriggerPulse] {
        &self.pulses
    }

    pub(crate) fn clear_retaining_capacity(&mut self) {
        self.pulses.clear();
    }

    #[cfg(test)]
    fn capacity(&self) -> usize {
        self.pulses.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::id::{EffectId, LayerId};

    fn pulse(sequence: u64) -> TriggerPulse {
        TriggerPulse {
            layer_id: Some(LayerId::new(format!("layer-{sequence}"))),
            owner_id: EffectId::new(format!("effect-{sequence}")),
            param_key: sequence,
            audio_stamp: None,
        }
    }

    fn stamped_pulse(
        sequence: u64,
        end_sample: u64,
        timeline_time: Option<Seconds>,
    ) -> TriggerPulse {
        let mut pulse = pulse(sequence);
        pulse.audio_stamp = Some(manifold_core::audio_features::AudioHopStamp {
            epoch: 17,
            end_sample,
            sample_rate: 48_000,
            source_time: None,
            timeline_time,
        });
        pulse
    }

    #[test]
    fn trigger_delivery_appends_sequence_and_preserves_clock_and_capacity() {
        let mut queue = TriggerDeliveryQueue::with_capacity(4);
        queue.reset().unwrap();
        let capacity = queue.capacity();
        let mut pulses = vec![pulse(1), pulse(2)];
        queue
            .append_batch(&mut pulses, Seconds(1.25), Beats(2.5))
            .unwrap();
        assert_eq!(queue.capacity(), capacity);
        assert_eq!(queue.as_slice()[0].sequence, 0);
        assert_eq!(queue.as_slice()[1].sequence, 1);
        assert_eq!(queue.as_slice()[0].accepted_time, Seconds(1.25));
        assert_eq!(queue.as_slice()[0].accepted_beat, Beats(2.5));
        assert_eq!(queue.as_slice()[0].epoch, 1);
        queue.clear_retaining_capacity();
        assert_eq!(queue.capacity(), capacity);
    }

    #[test]
    fn trigger_delivery_overflow_is_sticky_and_keeps_prefix() {
        let mut queue = TriggerDeliveryQueue::with_capacity(2);
        queue.reset().unwrap();
        let mut pulses = vec![pulse(1)];
        queue
            .append_batch(&mut pulses, Seconds::ZERO, Beats::ZERO)
            .unwrap();
        let mut overflowing = vec![pulse(2), pulse(3)];
        assert_eq!(
            queue.append_batch(&mut overflowing, Seconds::ZERO, Beats::ZERO,),
            Err(TriggerDeliveryError::CapacityOverflow)
        );
        assert_eq!(queue.as_slice().len(), 1);
        assert_eq!(
            overflowing.len(),
            2,
            "failed admission keeps the rejected batch intact"
        );
        let mut retry = vec![pulse(4)];
        assert_eq!(
            queue.append_batch(&mut retry, Seconds::ZERO, Beats::ZERO),
            Err(TriggerDeliveryError::CapacityOverflow)
        );
        assert_eq!(queue.failure().unwrap().epoch, 1);
        queue.reset().unwrap();
        assert!(queue.failure().is_none());
        assert!(queue.as_slice().is_empty());
    }

    #[test]
    fn trigger_delivery_rejects_invalid_clock_without_partial_append() {
        let mut queue = TriggerDeliveryQueue::with_capacity(2);
        queue.reset().unwrap();
        let mut pulses = vec![pulse(1), pulse(2)];
        assert_eq!(
            queue.append_batch(&mut pulses, Seconds(f64::NAN), Beats::ZERO),
            Err(TriggerDeliveryError::InvalidClock)
        );
        assert!(queue.as_slice().is_empty());
        assert_eq!(
            queue.failure().unwrap().kind,
            TriggerDeliveryError::InvalidClock
        );
    }

    #[test]
    fn trigger_delivery_preserves_equal_and_out_of_order_source_stamps() {
        let mut queue = TriggerDeliveryQueue::with_capacity(4);
        queue.reset().unwrap();
        let mut first = stamped_pulse(10, 2048, None);
        first.audio_stamp.as_mut().unwrap().source_time = Some(std::time::Instant::now());
        let second = stamped_pulse(11, 1024, Some(Seconds(8.0)));
        let mut pulses = vec![first.clone(), second.clone()];
        queue
            .append_batch(&mut pulses, Seconds(2.0), Beats(4.0))
            .unwrap();
        assert_eq!(queue.as_slice()[0].pulse.audio_stamp, first.audio_stamp);
        assert_eq!(queue.as_slice()[1].pulse.audio_stamp, second.audio_stamp);
        assert_eq!(queue.as_slice()[0].sequence, 0);
        assert_eq!(queue.as_slice()[1].sequence, 1);
    }

    #[test]
    fn trigger_delivery_sequence_and_epoch_exhaustion_do_not_wrap_or_erase_prefix() {
        let mut queue = TriggerDeliveryQueue::with_capacity(4);
        queue.reset().unwrap();
        queue
            .append_batch(&mut vec![pulse(1)], Seconds::ZERO, Beats::ZERO)
            .unwrap();
        queue.next_sequence = u64::MAX;
        let mut rejected = vec![pulse(2)];
        assert_eq!(
            queue.append_batch(&mut rejected, Seconds::ZERO, Beats::ZERO),
            Err(TriggerDeliveryError::SequenceExhausted)
        );
        assert_eq!(rejected.len(), 1);
        assert_eq!(queue.as_slice().len(), 1);
        queue.epoch = u64::MAX;
        assert_eq!(queue.reset(), Err(TriggerDeliveryError::EpochExhausted));
        assert_eq!(queue.as_slice().len(), 1);
        assert_eq!(queue.failure().unwrap().epoch, u64::MAX);
    }
}
