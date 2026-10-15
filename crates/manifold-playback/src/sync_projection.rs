//! The AbletonOSC and timecode sources' beat at the frame's read instant,
//! projected from their last report and its receive-thread arrival stamp
//! (FRAME_EXACT_TRANSPORT_DESIGN.md D7). Pure: the content thread and the
//! P3 proofs call the same functions.

use std::time::Instant;

/// Signed seconds from `from` to `to`.
pub fn seconds_between(from: Instant, to: Instant) -> f64 {
    if to >= from {
        (to - from).as_secs_f64()
    } else {
        -(from - to).as_secs_f64()
    }
}

/// One AbletonOSC song-time report, stamped on the bridge's receive thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongTimeReport {
    pub beat: f64,
    pub at: Instant,
}

/// Live's tempo as its listener reported it: `bpm` from `at`, `before` until
/// then.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TempoReport {
    pub before: f64,
    pub bpm: f64,
    pub at: Instant,
}

/// AbletonOSC: the reported beat advanced from its arrival to the read
/// instant at the tempo in force — `before` until the tempo report arrived,
/// `bpm` after. Ages are seconds before the read instant.
pub fn song_time_beat_at(beat: f64, report_age: f64, before: f64, bpm: f64, tempo_age: f64) -> f64 {
    let at_new = tempo_age.clamp(0.0, report_age.max(0.0));
    beat + ((report_age - at_new) * before + at_new * bpm) / 60.0
}

/// One timecode message, stamped on the OSC receiver's thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimecodeReport {
    /// Song seconds, offset applied.
    pub seconds: f64,
    pub at: Instant,
    /// The timecode frame's length when the message is an H:M:S:F label.
    pub frame: Option<f64>,
}

/// Song seconds at the read instant. A frame label means the frame has
/// started, so on average the true time is half a frame later.
pub fn timecode_seconds_at(seconds: f64, since_arrival: f64, frame: Option<f64>) -> f64 {
    seconds + frame.map_or(0.0, |f| 0.5 * f) + since_arrival
}

/// Beat and tempo at song seconds `s` through `map`: the tempo is the map's
/// over the next `step`, so a tempo point in the map is followed exactly.
pub fn map_sample(s: f64, step: f64, mut map: impl FnMut(f64) -> f64) -> (f64, f64) {
    let beat = map(s);
    (beat, 60.0 * (map(s + step) - beat) / step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn signed_seconds_both_ways() {
        let a = Instant::now();
        let b = a + Duration::from_millis(250);
        assert!((seconds_between(a, b) - 0.25).abs() < 1e-9);
        assert!((seconds_between(b, a) + 0.25).abs() < 1e-9);
    }

    #[test]
    fn song_time_advances_at_the_tempo_in_force() {
        // The tempo changed before the report: all of it at 128.
        let beat = song_time_beat_at(64.0, 0.030, 174.0, 128.0, 0.080);
        assert!((beat - (64.0 + 0.030 * 128.0 / 60.0)).abs() < 1e-12);
        // The report is 80 ms old, the tempo report 30 ms: 50 ms at 174, then 128.
        let beat = song_time_beat_at(64.0, 0.080, 174.0, 128.0, 0.030);
        assert!((beat - (64.0 + (0.050 * 174.0 + 0.030 * 128.0) / 60.0)).abs() < 1e-12);
    }

    #[test]
    fn frame_labels_gain_half_a_frame() {
        assert!((timecode_seconds_at(10.0, 0.02, Some(0.04)) - 10.04).abs() < 1e-12);
        assert!((timecode_seconds_at(10.0, 0.02, None) - 10.02).abs() < 1e-12);
    }

    #[test]
    fn map_tempo_is_the_next_frames() {
        // 120 BPM until 1 s, 60 BPM after: a step across the point reads the
        // average over the step, a step after it reads 60.
        let map = |s: f64| if s < 1.0 { 2.0 * s } else { 2.0 + (s - 1.0) };
        let (beat, bpm) = map_sample(0.5, 0.25, map);
        assert_eq!((beat, bpm), (1.0, 120.0));
        let (_, bpm) = map_sample(0.875, 0.25, map);
        assert!((bpm - 90.0).abs() < 1e-9);
        let (_, bpm) = map_sample(2.0, 0.25, map);
        assert!((bpm - 60.0).abs() < 1e-9);
    }
}
