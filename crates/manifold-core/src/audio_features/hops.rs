//! Completed audio hops retained across one content update. Producers reuse
//! these bounded buffers; each evaluator keeps its own allocation-free cursor.

use std::sync::atomic::{AtomicU64, Ordering};

use super::SendFeatures;
use crate::Seconds;

static NEXT_ANALYSIS_EPOCH: AtomicU64 = AtomicU64::new(1);

/// Mint an identity when constructing an analyzer, never once per audio hop.
pub fn new_audio_analysis_epoch() -> u64 {
    NEXT_ANALYSIS_EPOCH
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .expect("audio analysis epoch space exhausted")
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioHopStamp {
    pub epoch: u64,
    /// Exclusive received-mono sample boundary since analyzer construction.
    pub end_sample: u64,
    pub sample_rate: u32,
    /// Monotonic source time at the exclusive hop boundary, when retained by
    /// the producer. Independent of display delivery and transport mapping.
    pub source_time: Option<std::time::Instant>,
    /// Present only when the producer has an actual transport mapping. Live
    /// mixed audio must not infer this from the display frame receiving it.
    pub timeline_time: Option<Seconds>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioFeatureHop {
    pub stamp: AudioHopStamp,
    pub dt: Seconds,
    pub features: SendFeatures,
}

/// A completed analysis sample retained in an [`AudioHopBatch`].
pub trait AudioHopSample {
    fn stamp(&self) -> AudioHopStamp;
    fn duration(&self) -> Seconds;
    fn is_finite(&self) -> bool;
}

impl AudioHopSample for AudioFeatureHop {
    fn stamp(&self) -> AudioHopStamp {
        self.stamp
    }

    fn duration(&self) -> Seconds {
        self.dt
    }

    fn is_finite(&self) -> bool {
        features_are_finite(&self.features)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioHopError {
    InvalidInput,
    CapacityExceeded,
}

impl std::fmt::Display for AudioHopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "audio analysis input is invalid or out of order",
            Self::CapacityExceeded => {
                "audio analysis could not retain every event in this interval"
            }
        })
    }
}

impl std::error::Error for AudioHopError {}

/// This update's per-send hops. Exhaustion/invalid input latches for the
/// analyzer epoch; no valid-looking partial batch is exposed after a failure.
#[derive(Debug, PartialEq)]
pub struct AudioHopBatch<T: AudioHopSample = AudioFeatureHop> {
    epoch: u64,
    hops: Vec<T>,
    limit: usize,
    last_end: Option<u64>,
    last_source_time: Option<std::time::Instant>,
    sample_rate: Option<u32>,
    failure: Option<AudioHopError>,
}

impl<T: AudioHopSample + Clone> Clone for AudioHopBatch<T> {
    fn clone(&self) -> Self {
        let mut hops = Vec::with_capacity(self.limit);
        hops.extend(self.hops.iter().cloned());
        Self {
            epoch: self.epoch,
            hops,
            limit: self.limit,
            last_end: self.last_end,
            last_source_time: self.last_source_time,
            sample_rate: self.sample_rate,
            failure: self.failure,
        }
    }
}

impl<T: AudioHopSample> Default for AudioHopBatch<T> {
    fn default() -> Self {
        Self::with_capacity(512)
    }
}

impl<T: AudioHopSample> AudioHopBatch<T> {
    pub fn with_capacity(limit: usize) -> Self {
        assert!(limit > 0);
        Self {
            epoch: 0,
            hops: Vec::with_capacity(limit),
            limit,
            last_end: None,
            last_source_time: None,
            sample_rate: None,
            failure: None,
        }
    }

    pub fn begin(&mut self, epoch: u64) {
        self.hops.clear();
        if epoch != self.epoch {
            self.reset(epoch);
        }
    }

    /// Clear all stream validation state while retaining the backing storage.
    /// This is used when the selected source changes without a new analyzer
    /// epoch.
    pub fn reset(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.hops.clear();
        self.last_end = None;
        self.last_source_time = None;
        self.sample_rate = None;
        self.failure = None;
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn hops(&self) -> &[T] {
        &self.hops
    }
    pub fn failure(&self) -> Option<AudioHopError> {
        self.failure
    }

    /// Invalidate an interrupted producer interval without publishing a partial
    /// prefix. A new analyzer epoch is required before this batch can recover.
    pub fn invalidate(&mut self, error: AudioHopError) {
        self.hops.clear();
        self.failure.get_or_insert(error);
    }

    pub fn push(&mut self, hop: T) -> Result<(), AudioHopError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        let stamp = hop.stamp();
        let error = if self.epoch == 0
            || stamp.epoch != self.epoch
            || stamp.end_sample == 0
            || stamp.sample_rate == 0
            || self
                .sample_rate
                .is_some_and(|rate| rate != stamp.sample_rate)
            || !hop.duration().0.is_finite()
            || hop.duration().0 <= 0.0
            || stamp.timeline_time.is_some_and(|time| !time.0.is_finite())
            || !hop.is_finite()
            || self.last_end.is_some_and(|end| stamp.end_sample <= end)
            || self.last_source_time.zip(stamp.source_time).is_some_and(|(last, time)| time <= last)
        {
            Some(AudioHopError::InvalidInput)
        } else if self.hops.len() == self.limit {
            Some(AudioHopError::CapacityExceeded)
        } else {
            None
        };
        if let Some(error) = error {
            self.invalidate(error);
            return Err(error);
        }
        self.last_end = Some(stamp.end_sample);
        if let Some(time) = stamp.source_time { self.last_source_time = Some(time); }
        self.sample_rate = Some(stamp.sample_rate);
        self.hops.push(hop);
        Ok(())
    }
}

/// Per-evaluator progress, so param modulation and clip triggers may read the
/// same immutable batch independently without replaying it on a redraw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioHopCursor {
    epoch: u64,
    end_sample: u64,
}

impl AudioHopCursor {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Observe an announced epoch even before its first completed hop, so an
    /// older batch cannot revive the previous source during analyzer startup.
    pub fn begin_epoch(&mut self, epoch: u64) -> bool {
        if epoch <= self.epoch {
            return false;
        }
        self.epoch = epoch;
        self.end_sample = 0;
        true
    }

    /// Some(true) starts a new source epoch (reset follower/edge state),
    /// Some(false) advances the same stream, None ignores duplicate/stale input.
    pub fn accept(&mut self, stamp: AudioHopStamp) -> Option<bool> {
        if stamp.epoch == 0
            || stamp.epoch < self.epoch
            || (stamp.epoch == self.epoch && stamp.end_sample <= self.end_sample)
        {
            return None;
        }
        let new_epoch = stamp.epoch != self.epoch;
        self.epoch = stamp.epoch;
        self.end_sample = stamp.end_sample;
        Some(new_epoch)
    }
}

fn features_are_finite(features: &SendFeatures) -> bool {
    [
        features.pitch_hz,
        features.pitch_delta_st,
        features.pitch_confidence,
    ]
    .into_iter()
    .all(f32::is_finite)
        && features.bands.iter().all(|band| {
            [
                band.amplitude,
                band.brightness,
                band.noisiness,
                band.liveliness,
                band.transients,
                band.kick,
                band.pitch,
                band.presence,
            ]
            .into_iter()
            .all(f32::is_finite)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hop(epoch: u64, end_sample: u64) -> AudioFeatureHop {
        AudioFeatureHop {
            stamp: AudioHopStamp {
                epoch,
                end_sample,
                sample_rate: 48_000,
                source_time: None,
                timeline_time: Some(Seconds(end_sample as f64 / 48_000.)),
            },
            dt: Seconds(512. / 48_000.),
            features: SendFeatures::default(),
        }
    }

    #[test]
    fn source_clock_regression_across_updates_invalidates_until_new_epoch() {
        let now = std::time::Instant::now();
        let mut batch = AudioHopBatch::with_capacity(4);
        batch.begin(1);
        let mut first = hop(1, 512);
        first.stamp.source_time = Some(now);
        batch.push(first).unwrap();
        batch.begin(1);
        batch.push(hop(1, 1024)).unwrap(); // unknown does not forget last clock
        let mut backwards = hop(1, 1536);
        backwards.stamp.source_time = now.checked_sub(std::time::Duration::from_millis(1));
        assert_eq!(batch.push(backwards), Err(AudioHopError::InvalidInput));
        assert!(batch.hops().is_empty());
        batch.begin(2);
        backwards.stamp.epoch = 2;
        batch.push(backwards).unwrap();
    }

    #[test]
    fn two_consumers_each_read_the_batch_once_across_redraws() {
        let mut batch = AudioHopBatch::with_capacity(4);
        batch.begin(1);
        batch.push(hop(1, 512)).unwrap();
        batch.push(hop(1, 1024)).unwrap();
        for mut cursor in [AudioHopCursor::default(); 2] {
            assert_eq!(cursor.accept(batch.hops()[0].stamp), Some(true));
            assert_eq!(cursor.accept(batch.hops()[1].stamp), Some(false));
            for sample in batch.hops() {
                assert_eq!(cursor.accept(sample.stamp), None);
            }
            assert_eq!(cursor.accept(hop(2, 512).stamp), Some(true));
            assert_eq!(cursor.accept(hop(1, 2048).stamp), None);
        }
    }

    #[test]
    fn overflow_invalidates_the_whole_batch_until_new_epoch() {
        let mut batch = AudioHopBatch::with_capacity(1);
        batch.begin(1);
        batch.push(hop(1, 512)).unwrap();
        assert_eq!(
            batch.push(hop(1, 1024)),
            Err(AudioHopError::CapacityExceeded)
        );
        assert!(batch.hops().is_empty());
        batch.begin(1);
        assert_eq!(
            batch.push(hop(1, 1536)),
            Err(AudioHopError::CapacityExceeded)
        );
        batch.begin(2);
        batch.push(hop(2, 512)).unwrap();
    }

    #[test]
    fn malformed_order_and_timing_are_rejected() {
        let mut batch = AudioHopBatch::with_capacity(4);
        batch.begin(1);
        batch.push(hop(1, 512)).unwrap();
        batch.begin(1);
        assert_eq!(batch.push(hop(1, 512)), Err(AudioHopError::InvalidInput));
        batch.begin(2);
        let mut invalid = hop(2, 1024);
        invalid.dt = Seconds(f64::NAN);
        assert_eq!(batch.push(invalid), Err(AudioHopError::InvalidInput));
    }

    #[test]
    fn nonfinite_features_and_rate_changes_require_a_new_epoch() {
        let mut batch = AudioHopBatch::with_capacity(4);
        batch.begin(1);
        batch.push(hop(1, 512)).unwrap();
        let mut changed_rate = hop(1, 1024);
        changed_rate.stamp.sample_rate = 44_100;
        assert_eq!(batch.push(changed_rate), Err(AudioHopError::InvalidInput));
        assert!(batch.hops().is_empty());
        batch.begin(2);
        let mut invalid = hop(2, 512);
        invalid.features.bands[3].transients = f32::NAN;
        assert_eq!(batch.push(invalid), Err(AudioHopError::InvalidInput));
        batch.begin(3);
        batch.push(hop(3, 512)).unwrap();
    }

    #[derive(Clone, Debug, PartialEq)]
    struct GenericHop {
        stamp: AudioHopStamp,
        dt: Seconds,
        finite: bool,
    }

    impl AudioHopSample for GenericHop {
        fn stamp(&self) -> AudioHopStamp {
            self.stamp
        }

        fn duration(&self) -> Seconds {
            self.dt
        }

        fn is_finite(&self) -> bool {
            self.finite
        }
    }

    fn generic_hop(epoch: u64, end_sample: u64, finite: bool) -> GenericHop {
        GenericHop {
            stamp: AudioHopStamp {
                epoch,
                end_sample,
                sample_rate: 48_000,
                source_time: None,
                timeline_time: None,
            },
            dt: Seconds(512. / 48_000.),
            finite,
        }
    }

    #[test]
    fn generic_samples_apply_finite_payload_validation() {
        let mut batch = AudioHopBatch::<GenericHop>::with_capacity(2);
        batch.begin(1);
        assert_eq!(
            batch.push(generic_hop(1, 512, false)),
            Err(AudioHopError::InvalidInput)
        );
        assert!(batch.hops().is_empty());
    }

    #[test]
    fn reset_clears_sticky_state_and_preserves_capacity() {
        let mut batch = AudioHopBatch::with_capacity(2);
        batch.begin(1);
        batch.push(hop(1, 512)).unwrap();
        let capacity = batch.hops.capacity();
        assert_eq!(
            batch.push(hop(1, 1024)),
            Ok(()),
            "capacity two accepts the second hop"
        );
        batch.invalidate(AudioHopError::CapacityExceeded);
        batch.reset(1);
        assert_eq!(batch.failure(), None);
        assert_eq!(batch.hops.capacity(), capacity);
        batch.push(hop(1, 512)).unwrap();
    }

    #[test]
    fn clone_preserves_bound_order_and_capacity_for_future_appends() {
        let mut batch = AudioHopBatch::with_capacity(2);
        batch.begin(1);
        batch.push(hop(1, 512)).unwrap();
        let mut cloned = batch.clone();
        assert_eq!(cloned.hops(), batch.hops());
        assert_eq!(cloned.hops.capacity(), batch.hops.capacity());

        cloned.push(hop(1, 1024)).unwrap();
        assert_eq!(cloned.hops.capacity(), 2);
        assert_eq!(
            cloned.push(hop(1, 1536)),
            Err(AudioHopError::CapacityExceeded)
        );
        assert_eq!(cloned.hops.capacity(), 2);
        assert!(cloned.hops().is_empty());
    }

    #[test]
    fn empty_new_epoch_rejects_late_samples_from_previous_source() {
        let mut cursor = AudioHopCursor::default();
        assert_eq!(cursor.accept(hop(1, 512).stamp), Some(true));
        assert!(cursor.begin_epoch(2));
        assert_eq!(cursor.accept(hop(1, 1024).stamp), None);
        assert_eq!(cursor.accept(hop(2, 512).stamp), Some(false));
    }
}
