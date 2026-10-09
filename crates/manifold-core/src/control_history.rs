//! Bounded control observations retained for one content update.

use crate::audio_features::{AudioHopError, AudioHopStamp};
use crate::{Beats, ClipId, LayerId, Seconds};

/// Provenance of a control event. Clip events remain in beats until their
/// consumer resolves them against the current tempo map.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerSourceStamp {
    Snapshot,
    Audio {
        stamp: AudioHopStamp,
        time: Seconds,
    },
    Clip {
        layer_id: LayerId,
        clip_id: ClipId,
        beat: Beats,
    },
}

/// The contribution a control source made during one observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlContribution {
    Continuous(f32),
    Stepped(Option<f32>),
    TriggerCounter { count: u32, value: f32 },
}

/// A typed audio or clip control observation retained for downstream readers.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlObservation {
    pub stamp: TriggerSourceStamp,
    pub time: Seconds,
    pub dt: Seconds,
    pub contribution: ControlContribution,
    pub evaluation_time: Option<Seconds>,
}

/// Errors latch until the history is reset for a new source epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlHistoryError {
    InvalidInput,
    CapacityExceeded,
    Audio(AudioHopError),
}

impl std::fmt::Display for ControlHistoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput => f.write_str("control history input is invalid or out of order"),
            Self::CapacityExceeded => {
                f.write_str("control history could not retain every event in this interval")
            }
            Self::Audio(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ControlHistoryError {}

impl From<AudioHopError> for ControlHistoryError {
    fn from(error: AudioHopError) -> Self {
        Self::Audio(error)
    }
}

/// Reusable, bounded storage for the control observations of one update.
#[derive(Debug, PartialEq)]
pub struct ControlHistory {
    epoch: u64,
    observations: Vec<ControlObservation>,
    limit: usize,
    last_time: Option<Seconds>,
    failure: Option<ControlHistoryError>,
}

impl Clone for ControlHistory {
    fn clone(&self) -> Self {
        let mut observations = Vec::with_capacity(self.limit);
        observations.extend(self.observations.iter().cloned());
        Self {
            epoch: self.epoch,
            observations,
            limit: self.limit,
            last_time: self.last_time,
            failure: self.failure,
        }
    }
}

impl Default for ControlHistory {
    fn default() -> Self {
        Self::with_capacity(512)
    }
}

impl ControlHistory {
    pub fn with_capacity(limit: usize) -> Self {
        assert!(limit > 0, "control history capacity must be positive");
        Self {
            epoch: 0,
            observations: Vec::with_capacity(limit),
            limit,
            last_time: None,
            failure: None,
        }
    }

    /// Start an update, retaining the failure latch unless the source epoch
    /// changed. Transport ordering is per update, so its cursor is cleared on
    /// every begin.
    pub fn begin(&mut self, epoch: u64) {
        self.observations.clear();
        self.last_time = None;
        if epoch != self.epoch {
            self.reset(epoch);
        }
    }

    /// Explicitly clear all history and validation state for an epoch.
    pub fn reset(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.observations.clear();
        self.last_time = None;
        self.failure = None;
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn observations(&self) -> &[ControlObservation] {
        &self.observations
    }

    /// The logical retention limit, independent of the Vec's allocator state.
    pub fn capacity(&self) -> usize {
        self.limit
    }

    pub fn failure(&self) -> Option<ControlHistoryError> {
        self.failure
    }

    /// Invalidate an interval without exposing an accepted prefix.
    pub fn invalidate(&mut self, error: ControlHistoryError) {
        self.observations.clear();
        self.failure.get_or_insert(error);
    }

    pub fn push(&mut self, observation: ControlObservation) -> Result<(), ControlHistoryError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if !observation_is_valid(&observation)
            || self
                .last_time
                .is_some_and(|last_time| observation.time < last_time)
        {
            self.invalidate(ControlHistoryError::InvalidInput);
            return Err(ControlHistoryError::InvalidInput);
        }
        if self.observations.len() == self.limit {
            self.invalidate(ControlHistoryError::CapacityExceeded);
            return Err(ControlHistoryError::CapacityExceeded);
        }
        self.last_time = Some(observation.time);
        self.observations.push(observation);
        Ok(())
    }
}

fn observation_is_valid(observation: &ControlObservation) -> bool {
    if !observation.time.0.is_finite()
        || !observation.dt.0.is_finite()
        || observation.dt.0 < 0.0
        || observation.evaluation_time.is_some_and(|time| !time.0.is_finite())
        || !contribution_is_finite(observation.contribution)
    {
        return false;
    }

    match &observation.stamp {
        TriggerSourceStamp::Snapshot => true,
        TriggerSourceStamp::Audio { stamp, time } => {
            observation.dt.0 > 0.0
                && time.0.is_finite()
                && time == &observation.time
                && stamp.epoch != 0
                && stamp.end_sample > 0
                && stamp.sample_rate > 0
                && stamp.timeline_time.is_none_or(|mapped| mapped.0.is_finite())
        }
        TriggerSourceStamp::Clip { beat, .. } => {
            observation.dt.0 == 0.0 && beat.0.is_finite()
        }
    }
}

fn contribution_is_finite(contribution: ControlContribution) -> bool {
    match contribution {
        ControlContribution::Continuous(value)
        | ControlContribution::TriggerCounter { value, .. } => value.is_finite(),
        ControlContribution::Stepped(value) => value.is_none_or(f32::is_finite),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(time: f64, id: &str) -> ControlObservation {
        ControlObservation {
            stamp: TriggerSourceStamp::Clip {
                layer_id: LayerId::new("layer"),
                clip_id: ClipId::new(id),
                beat: Beats(time),
            },
            time: Seconds(time),
            dt: Seconds(0.0),
            contribution: ControlContribution::Stepped(Some(0.5)),
            evaluation_time: None,
        }
    }

    fn audio(time: f64, end_sample: u64) -> ControlObservation {
        ControlObservation {
            stamp: TriggerSourceStamp::Audio {
                stamp: AudioHopStamp {
                    epoch: 1,
                    end_sample,
                    sample_rate: 48_000,
                    source_time: None,
                    timeline_time: Some(Seconds(time)),
                },
                time: Seconds(time),
            },
            time: Seconds(time),
            dt: Seconds(1.0 / 48_000.0),
            contribution: ControlContribution::Continuous(0.5),
            evaluation_time: Some(Seconds(time)),
        }
    }

    #[test]
    fn equal_time_clip_events_are_distinct_and_ordered() {
        let mut history = ControlHistory::with_capacity(2);
        history.begin(0);
        history.push(clip(1.0, "a")).unwrap();
        history.push(clip(1.0, "b")).unwrap();
        assert_eq!(history.observations().len(), 2);
        assert_eq!(history.observations()[0].stamp, clip(1.0, "a").stamp);
        assert_eq!(history.observations()[1].stamp, clip(1.0, "b").stamp);
    }

    #[test]
    fn invalid_and_overflow_inputs_clear_prefix_and_latch_until_reset() {
        let mut history = ControlHistory::with_capacity(2);
        history.begin(1);
        history.push(audio(1.0, 1)).unwrap();
        let mut backwards = clip(0.5, "backwards");
        backwards.time = Seconds(0.5);
        assert_eq!(history.push(backwards), Err(ControlHistoryError::InvalidInput));
        assert!(history.observations().is_empty());
        assert_eq!(history.push(clip(2.0, "latched")), Err(ControlHistoryError::InvalidInput));

        history.reset(1);
        history.push(audio(1.0, 1)).unwrap();
        history.push(audio(2.0, 2)).unwrap();
        assert_eq!(history.push(audio(3.0, 3)), Err(ControlHistoryError::CapacityExceeded));
        assert!(history.observations().is_empty());
        history.begin(1);
        assert_eq!(history.push(audio(4.0, 4)), Err(ControlHistoryError::CapacityExceeded));
        history.reset(2);
        history.push(clip(0.0, "recovered")).unwrap();
    }

    #[test]
    fn capacity_is_reused_and_clone_preserves_logical_limit() {
        let mut history = ControlHistory::with_capacity(3);
        history.begin(0);
        history.push(clip(0.0, "a")).unwrap();
        let clone = history.clone();
        assert_eq!(clone.capacity(), 3);
        assert_eq!(clone.observations(), history.observations());
        history.reset(0);
        assert_eq!(history.capacity(), 3);
        history.push(clip(0.0, "b")).unwrap();
        assert_eq!(history.observations().len(), 1);
    }
}
