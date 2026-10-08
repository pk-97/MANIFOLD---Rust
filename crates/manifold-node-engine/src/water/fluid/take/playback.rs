use std::path::PathBuf;
use std::sync::Arc;

use manifold_core::Seconds;

use super::{Hash, TakeRange, TakeTime, batch_path, interpolate_time, read_record};

pub(super) const MAX_CLOCK_INDEX_ENTRIES: usize = 1_048_576;

/// The preflight digest and final transport coordinate for one nonempty clock
/// record. The batch itself is loaded only when a lookup selects this entry.
pub(super) struct ClockIndexEntry {
    pub(super) record: u64,
    pub(super) digest: Hash,
    pub(super) last_transport: Seconds,
}

struct CachedClockRecord {
    record: u64,
    points: Vec<TakeTime>,
}

/// Authenticated random access to the project clock of a committed take.
/// Inputs are deliberately not retained: only the selected record's clock
/// points remain cached between lookups. This is the verified prefix snapshot,
/// not a live watcher for subsequent journal edits or appended records.
pub(crate) struct PlaybackClock {
    directory: Arc<PathBuf>,
    range: TakeRange,
    index: Vec<ClockIndexEntry>,
    cached: Option<CachedClockRecord>,
}

impl PlaybackClock {
    pub(super) fn new(
        directory: Arc<PathBuf>,
        range: TakeRange,
        index: Vec<ClockIndexEntry>,
    ) -> Self {
        Self {
            directory,
            range,
            index,
            cached: None,
        }
    }

    pub(crate) fn simulation_time_at(&mut self, transport: Seconds) -> Result<Seconds, String> {
        let value = transport.0;
        let start = self.range.start.transport.0;
        let end = self.range.end.transport.0;
        if !value.is_finite() || value < start || value > end {
            return Err(
                "Physics take: requested time is outside the completed project range".into(),
            );
        }
        if value == start {
            return Ok(self.range.start.simulation);
        }

        let entry_index = self
            .index
            .partition_point(|entry| entry.last_transport.0 < value);
        let entry = self.index.get(entry_index).ok_or_else(|| {
            "Physics take: recorded project time has no simulation boundary".to_owned()
        })?;

        if self
            .cached
            .as_ref()
            .is_none_or(|cached| cached.record != entry.record)
        {
            let (batch, digest): (super::Batch, Hash) =
                read_record(&batch_path(&self.directory, entry.record))?;
            if digest != entry.digest {
                return Err("Physics take: project timing changed after clock preflight".into());
            }
            self.cached = Some(CachedClockRecord {
                record: entry.record,
                points: batch.clock,
            });
        }

        let points = &self
            .cached
            .as_ref()
            .expect("selected clock record is cached")
            .points;
        let after = points.partition_point(|point| point.transport.0 < value);
        let point = *points
            .get(after)
            .ok_or("Physics take: recorded project time has no simulation boundary")?;
        let previous = after.checked_sub(1).map_or(point, |before| points[before]);
        let width = point.transport.0 - previous.transport.0;
        Ok(if width == 0.0 {
            point.simulation
        } else {
            interpolate_time(previous, point, (value - previous.transport.0) / width).simulation
        })
    }
}
