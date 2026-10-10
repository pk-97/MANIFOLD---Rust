//! Bounded storage for modulation trigger events.

/// The kind of trigger represented by a [`TriggerPulse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerPulseKind {
    /// The legacy layer or modifier gate stream.
    Gate,
    /// A named `Fire` parameter that must not advance layer or master gate
    /// counters.
    Parameter,
}

pub use manifold_core::control_history::TriggerSourceStamp;

/// The existing modulation event payload, distinguishing gate events from
/// named parameter events.
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerPulse {
    pub kind: TriggerPulseKind,
    /// `None` denotes a master-chain instance.
    pub layer_id: Option<manifold_core::LayerId>,
    /// Preserve the firing parameter's owner through renderer dispatch.
    pub owner_id: manifold_core::EffectId,
    /// Allocation-free parameter token; owner identity is carried separately.
    pub param_key: u64,
    /// Provenance of the source event; destination identity remains in the
    /// fields above.
    pub source_stamp: TriggerSourceStamp,
}

impl TriggerPulse {
    /// Compatibility view for consumers that only need audio-hop provenance.
    pub fn audio_stamp(&self) -> Option<manifold_core::audio_features::AudioHopStamp> {
        match &self.source_stamp {
            TriggerSourceStamp::Audio { stamp, .. } => Some(*stamp),
            TriggerSourceStamp::Snapshot | TriggerSourceStamp::Clip { .. } => None,
        }
    }
}

/// Destination for modulation trigger events.
pub trait TriggerPulseSink {
    fn clear_events(&mut self);
    fn push_event(&mut self, pulse: TriggerPulse);
}

/// A caller-managed, bounded event buffer for the modulation engine.
///
/// Once full, the buffer retains its accepted prefix and latches overflow.
/// Callers must report the overflow and must not present that prefix as a
/// complete event stream.
#[derive(Debug, Default)]
pub struct TriggerPulseBuffer {
    pulses: Vec<TriggerPulse>,
    limit: usize,
    overflowed: bool,
}

impl TriggerPulseBuffer {
    /// Construct a buffer with storage reserved for at most `limit` events.
    pub fn with_capacity(limit: usize) -> Self {
        assert!(limit > 0, "trigger buffer capacity must be positive");
        Self {
            pulses: Vec::with_capacity(limit),
            limit,
            overflowed: false,
        }
    }

    /// Whether an event was rejected because the buffer reached its limit.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Remove accepted events and clear the overflow latch while retaining the
    /// buffer's allocation for reuse.
    pub fn clear(&mut self) {
        self.pulses.clear();
        self.overflowed = false;
    }

    pub fn is_empty(&self) -> bool {
        self.pulses.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pulses.len()
    }

    pub fn capacity(&self) -> usize {
        self.pulses.capacity()
    }

    /// Mutable access used only by the engine delivery queue to drain accepted
    /// pulses. The queue must inspect [`Self::overflowed`] before treating the
    /// drained events as complete.
    pub(crate) fn pulses_mut(&mut self) -> &mut Vec<TriggerPulse> {
        &mut self.pulses
    }
}

// Compatibility for callers that manage their own storage. PlaybackEngine uses
// TriggerPulseBuffer so a dense input interval cannot grow its event scratch.
impl TriggerPulseSink for Vec<TriggerPulse> {
    fn clear_events(&mut self) {
        self.clear();
    }

    fn push_event(&mut self, pulse: TriggerPulse) {
        self.push(pulse);
    }
}

impl TriggerPulseSink for TriggerPulseBuffer {
    fn clear_events(&mut self) {
        self.clear();
    }

    fn push_event(&mut self, pulse: TriggerPulse) {
        if self.overflowed {
            return;
        }
        if self.pulses.len() == self.limit {
            self.overflowed = true;
            return;
        }
        self.pulses.push(pulse);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_features::AudioHopStamp;
    use manifold_core::{EffectId, LayerId};

    fn pulse(
        kind: TriggerPulseKind,
        layer: Option<&str>,
        owner: &str,
        key: u64,
        sample: u64,
    ) -> TriggerPulse {
        TriggerPulse {
            kind,
            layer_id: layer.map(LayerId::new),
            owner_id: EffectId::new(owner),
            param_key: key,
            source_stamp: TriggerSourceStamp::Audio {
                stamp: AudioHopStamp {
                epoch: 7,
                end_sample: sample,
                sample_rate: 48_000,
                source_time: None,
                timeline_time: None,
                },
                time: manifold_core::Seconds::ZERO,
            },
        }
    }

    #[test]
    fn preserves_mixed_event_identity_stamp_and_order() {
        let first = pulse(TriggerPulseKind::Gate, Some("layer"), "gate", 1, 128);
        let second = pulse(TriggerPulseKind::Parameter, None, "param", 2, 256);
        let mut buffer = TriggerPulseBuffer::with_capacity(2);
        buffer.push_event(first.clone());
        buffer.push_event(second.clone());

        assert_eq!(buffer.pulses_mut(), &[first, second]);
        assert!(!buffer.overflowed());
    }

    #[test]
    fn overflow_retains_prefix_and_capacity_until_clear() {
        let mut buffer = TriggerPulseBuffer::with_capacity(2);
        let first = pulse(TriggerPulseKind::Gate, Some("layer"), "owner", 1, 128);
        let second = pulse(TriggerPulseKind::Parameter, None, "owner", 2, 256);
        buffer.push_event(first.clone());
        buffer.push_event(second.clone());
        let capacity = buffer.capacity();
        buffer.push_event(pulse(TriggerPulseKind::Gate, None, "owner", 3, 384));
        for sample in 4..64 {
            buffer.push_event(pulse(
                TriggerPulseKind::Parameter,
                None,
                "owner",
                sample,
                sample * 128,
            ));
        }

        assert!(buffer.overflowed());
        assert_eq!(buffer.pulses_mut(), &[first, second]);
        assert_eq!(buffer.capacity(), capacity);
    }

    #[test]
    fn clear_reuses_storage_and_resets_overflow() {
        let mut buffer = TriggerPulseBuffer::with_capacity(1);
        buffer.push_event(pulse(TriggerPulseKind::Gate, None, "owner", 1, 128));
        buffer.push_event(pulse(TriggerPulseKind::Parameter, None, "owner", 2, 256));
        let capacity = buffer.capacity();
        buffer.clear();

        assert!(buffer.is_empty());
        assert!(!buffer.overflowed());
        assert_eq!(buffer.capacity(), capacity);
        buffer.push_event(pulse(TriggerPulseKind::Parameter, None, "owner", 3, 384));
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn mem_take_leaves_allocation_free_default_and_retains_extracted_storage() {
        let mut buffer = TriggerPulseBuffer::with_capacity(3);
        buffer.push_event(pulse(TriggerPulseKind::Gate, None, "owner", 1, 128));
        let capacity = buffer.capacity();
        let extracted = std::mem::take(&mut buffer);

        assert_eq!(extracted.capacity(), capacity);
        assert!(buffer.is_empty());
        assert_eq!(buffer.capacity(), 0);
        assert!(!buffer.overflowed());
    }
}
