use crate::math::BeatQuantizer;
use crate::types::TempoPointSource;
use crate::units::{Beats, Bpm, Seconds};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// A single tempo change point in the tempo map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoPoint {
    pub beat: Beats,
    pub bpm: Bpm,
    #[serde(default)]
    pub source: TempoPointSource,
    #[serde(default = "default_neg_one")]
    pub recorded_at_seconds: Seconds,
}

/// Beat-anchored tempo automation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TempoMap {
    #[serde(default)]
    points: Arc<Vec<TempoPoint>>,

    #[serde(skip)]
    is_sorted: bool,
}

impl TempoMap {
    pub fn ensure_sorted(&mut self) {
        if !self.is_sorted {
            if self.points.len() > 1 {
                Arc::make_mut(&mut self.points).sort_by(|a, b| {
                    a.beat
                        .partial_cmp(&b.beat)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            self.is_sorted = true;
        }
    }

    /// Validate and sanitize all tempo points.
    pub fn ensure_valid(&mut self) {
        // Remove any points with NaN or infinite BPM/beat
        Arc::make_mut(&mut self.points).retain(|p| p.bpm.0.is_finite() && p.beat.is_finite());
        // Clamp BPM to 20-300
        for p in Arc::make_mut(&mut self.points).iter_mut() {
            p.bpm = Bpm::clamped(p.bpm.0);
        }
        // Re-sort by beat
        Arc::make_mut(&mut self.points).sort_by(|a, b| {
            a.beat
                .partial_cmp(&b.beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        self.is_sorted = true;
    }

    /// Get BPM at a given beat (step-change lookup).
    /// Unity TempoMap.cs lines 198-214: initializes from points[0].bpm, not fallback.
    pub fn get_bpm_at_beat(&mut self, beat: Beats, fallback: Bpm) -> Bpm {
        self.ensure_sorted();
        if self.points.is_empty() {
            return Bpm::clamped(fallback.0);
        }
        let mut bpm = self.points[0].bpm;
        for point in self.points.iter() {
            if point.beat <= beat {
                bpm = point.bpm;
            } else {
                break;
            }
        }
        Bpm::clamped(bpm.0)
    }

    /// Immutable version of `get_bpm_at_beat` — allocation-free, order-independent.
    /// Returns the BPM at a given beat: the point with the maximum beat <= query,
    /// or the earliest-beat point's BPM if no such point exists (matching the &mut version's semantics).
    /// Clamped to 20..300 BPM. Returns `fallback` if the map is empty.
    pub fn get_bpm_at_beat_immut(&self, beat: Beats, fallback: Bpm) -> Bpm {
        if self.points.is_empty() {
            return Bpm::clamped(fallback.0);
        }
        // Track earliest beat (for initialization) and max beat <= query (for result)
        let mut earliest_beat = Beats(f64::MAX);
        let mut init_bpm = self.points[0].bpm;
        let mut max_beat_le_query = Beats(f64::MIN);
        let mut result_bpm = init_bpm;

        for point in self.points.iter() {
            // Track earliest point for initialization (matching &mut version's points[0].bpm after sort)
            if point.beat < earliest_beat {
                earliest_beat = point.beat;
                init_bpm = point.bpm;
            }
            // Track max-beat point <= query
            if point.beat <= beat && point.beat >= max_beat_le_query {
                max_beat_le_query = point.beat;
                result_bpm = point.bpm;
            }
        }

        // If no point satisfies beat <= query, use earliest point's BPM (matching &mut semantics)
        let result = if max_beat_le_query == Beats(f64::MIN) {
            init_bpm
        } else {
            result_bpm
        };
        Bpm::clamped(result.0)
    }

    pub fn add_or_replace_point(
        &mut self,
        beat: Beats,
        bpm: Bpm,
        source: TempoPointSource,
        epsilon: f32,
    ) {
        self.add_or_replace_point_with_time(beat, bpm, source, epsilon, Seconds(-1.0));
    }

    pub fn add_or_replace_point_with_time(
        &mut self,
        beat: Beats,
        bpm: Bpm,
        source: TempoPointSource,
        epsilon: f32,
        recorded_at_seconds: Seconds,
    ) {
        let beat = BeatQuantizer::quantize_beat(beat);
        let bpm = Bpm(BeatQuantizer::quantize_bpm(bpm.0));

        // Remove existing point at same beat (within epsilon)
        Arc::make_mut(&mut self.points)
            .retain(|p| (p.beat - beat).abs() > Beats::from_f32(epsilon));

        Arc::make_mut(&mut self.points).push(TempoPoint {
            beat,
            bpm,
            source,
            recorded_at_seconds,
        });
        self.is_sorted = false;
    }

    pub fn ensure_default_at_beat_zero(&mut self, fallback_bpm: Bpm, source: TempoPointSource) {
        self.ensure_sorted();
        if self.points.is_empty() || self.points[0].beat > Beats::from_f32(0.001) {
            self.add_or_replace_point(Beats::ZERO, fallback_bpm, source, 0.001);
        }
    }

    #[inline]
    pub fn points(&self) -> &[TempoPoint] {
        self.points.as_slice()
    }

    #[inline]
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Return whether two maps currently share their immutable point storage.
    #[inline]
    pub fn shares_points(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.points, &other.points)
    }

    pub fn clear(&mut self) {
        Arc::make_mut(&mut self.points).clear();
        self.is_sorted = true;
    }

    pub fn clone_points(&self) -> Vec<TempoPoint> {
        self.points.as_ref().clone()
    }

    pub fn get_sorted_points(&mut self) -> &[TempoPoint] {
        self.ensure_sorted();
        self.points.as_slice()
    }
}

/// Converts a clip's timeline span into the media seconds playback advances
/// over it. Every edit that moves a clip's left edge (trim, split, region cut,
/// overlap trim) advances `in_point` through this, so the retained media stays
/// on the beat it played on. Warped clips run at their recorded tempo;
/// unwarped clips follow the tempo map. Borrows fields rather than the whole
/// project so a write path can hold a layer mutably while using it.
#[derive(Debug, Clone, Copy)]
pub struct SourceClock<'a> {
    tempo_map: &'a TempoMap,
    fallback_bpm: Bpm,
    project_recorded_bpm: Option<Bpm>,
}

impl<'a> SourceClock<'a> {
    pub fn new(tempo_map: &'a TempoMap, fallback_bpm: Bpm, project_recorded_bpm: Option<Bpm>) -> Self {
        Self { tempo_map, fallback_bpm, project_recorded_bpm }
    }

    /// Media seconds `clip` advances while the timeline moves `from` → `to`.
    pub fn source_seconds(&self, clip: &crate::clip::TimelineClip, from: Beats, to: Beats) -> Seconds {
        let recorded_bpm = clip.resolve_recorded_bpm(self.project_recorded_bpm);
        if recorded_bpm > 0.0 {
            return Seconds((to - from).0 * 60.0 / f64::from(recorded_bpm));
        }
        let at = |beat| TempoMapConverter::beat_to_seconds_immut(self.tempo_map, beat, self.fallback_bpm);
        at(to) - at(from)
    }

    /// Inverse of [`Self::source_seconds`]: how many beats past `from` it
    /// takes `clip` to play `seconds` of media.
    pub fn beats_for_source(&self, clip: &crate::clip::TimelineClip, from: Beats, seconds: Seconds) -> Beats {
        let recorded_bpm = clip.resolve_recorded_bpm(self.project_recorded_bpm);
        if recorded_bpm > 0.0 {
            return Beats(seconds.0 * f64::from(recorded_bpm) / 60.0);
        }
        let start = TempoMapConverter::beat_to_seconds_immut(self.tempo_map, from, self.fallback_bpm);
        TempoMapConverter::seconds_to_beat_immut(self.tempo_map, start + seconds, self.fallback_bpm) - from
    }
}

/// Pure tempo math — beat↔seconds conversion via piecewise integration.
/// Port of Unity TempoMapConverter.cs.
pub struct TempoMapConverter;

impl TempoMapConverter {
    /// Unity TempoMapConverter.cs line 111-113: clamps BPM to 20-300.
    #[must_use]
    pub fn seconds_per_beat_from_bpm(bpm: f32) -> f32 {
        60.0 / bpm.clamp(20.0, 300.0)
    }

    /// Get BPM at beat 0 from tempo map, with fallback.
    /// Unity TempoMapConverter.cs lines 116-121.
    fn get_bpm_at_beat_zero(tempo_map: &TempoMap, fallback_bpm: Bpm) -> Bpm {
        if tempo_map.points.is_empty() {
            return Bpm::clamped(fallback_bpm.0);
        }
        let mut bpm = tempo_map.points[0].bpm;
        for point in tempo_map.points.iter() {
            if point.beat > Beats::ZERO {
                break;
            }
            bpm = point.bpm;
        }
        Bpm::clamped(bpm.0)
    }

    /// Convert beat position to seconds using tempo map.
    /// Unity TempoMapConverter.cs lines 14-56.
    #[must_use]
    pub fn beat_to_seconds(tempo_map: &mut TempoMap, beat: Beats, fallback_bpm: Bpm) -> Seconds {
        tempo_map.ensure_sorted();
        Self::beat_to_seconds_immut(tempo_map, beat, fallback_bpm)
    }

    /// Immutable version of beat_to_seconds. Assumes tempo map is already sorted
    /// (guaranteed after on_after_deserialize / ensure_valid).
    #[must_use]
    pub fn beat_to_seconds_immut(tempo_map: &TempoMap, beat: Beats, fallback_bpm: Bpm) -> Seconds {
        let bpm_at_zero = Self::get_bpm_at_beat_zero(tempo_map, fallback_bpm);
        let spb_at_zero = Self::seconds_per_beat_from_bpm_f64(bpm_at_zero.0);

        if tempo_map.points.is_empty() {
            return Seconds(beat.0 * spb_at_zero);
        }

        if beat <= Beats::ZERO {
            return Seconds(beat.0 * spb_at_zero);
        }

        let mut total_seconds = 0.0_f64;
        let mut current_beat = 0.0_f64;
        let mut current_bpm = bpm_at_zero.0;

        for point in tempo_map.points.iter() {
            if point.beat <= Beats::ZERO {
                current_bpm = point.bpm.0;
                continue;
            }
            if point.beat >= beat {
                break;
            }
            let delta_beats = point.beat.0 - current_beat;
            if delta_beats > 0.0 {
                total_seconds += delta_beats * Self::seconds_per_beat_from_bpm_f64(current_bpm);
            }
            current_beat = point.beat.0;
            current_bpm = point.bpm.0;
        }

        let tail_beats = beat.0 - current_beat;
        if tail_beats > 0.0 {
            total_seconds += tail_beats * Self::seconds_per_beat_from_bpm_f64(current_bpm);
        }

        Seconds(total_seconds)
    }

    /// Convert seconds to beat position using tempo map.
    /// Unity TempoMapConverter.cs lines 61-109.
    #[must_use]
    pub fn seconds_to_beat(tempo_map: &mut TempoMap, seconds: Seconds, fallback_bpm: Bpm) -> Beats {
        tempo_map.ensure_sorted();
        Self::seconds_to_beat_immut(tempo_map, seconds, fallback_bpm)
    }

    /// Immutable seconds-to-beats conversion using f64 arithmetic throughout.
    /// Assumes the map is already sorted by ensure_sorted / ensure_valid.
    #[must_use]
    pub fn seconds_to_beat_immut(
        tempo_map: &TempoMap,
        seconds: Seconds,
        fallback_bpm: Bpm,
    ) -> Beats {
        let bpm_at_zero = Self::get_bpm_at_beat_zero(tempo_map, fallback_bpm);
        let spb_at_zero = Self::seconds_per_beat_from_bpm_f64(bpm_at_zero.0);

        if tempo_map.points.is_empty() || seconds <= Seconds::ZERO {
            return if spb_at_zero > 0.0 {
                Beats(seconds.0 / spb_at_zero)
            } else {
                Beats::ZERO
            };
        }

        let mut remaining_seconds = seconds.0;
        let mut current_beat = 0.0_f64;
        let mut current_bpm = bpm_at_zero.0;

        for point in tempo_map.points.iter() {
            // Skip points at or before beat 0 (absorb their BPM)
            if point.beat <= Beats::ZERO {
                current_bpm = point.bpm.0;
                continue;
            }

            let segment_beats = point.beat.0 - current_beat;
            if segment_beats <= 0.0 {
                current_beat = point.beat.0;
                current_bpm = point.bpm.0;
                continue;
            }

            let segment_seconds = segment_beats * Self::seconds_per_beat_from_bpm_f64(current_bpm);
            if remaining_seconds <= segment_seconds {
                let spb = Self::seconds_per_beat_from_bpm_f64(current_bpm);
                return if spb > 0.0 {
                    Beats(current_beat + remaining_seconds / spb)
                } else {
                    Beats(current_beat)
                };
            }

            remaining_seconds -= segment_seconds;
            current_beat = point.beat.0;
            current_bpm = point.bpm.0;
        }

        let tail_spb = Self::seconds_per_beat_from_bpm_f64(current_bpm);
        if tail_spb > 0.0 {
            Beats(current_beat + remaining_seconds / tail_spb)
        } else {
            Beats(current_beat)
        }
    }

    // Position conversions keep f64 precision throughout long shows.
    #[must_use]
    fn seconds_per_beat_from_bpm_f64(bpm: f32) -> f64 {
        60.0_f64 / (bpm.clamp(20.0, 300.0) as f64)
    }

    /// Convert seconds to beat position using tempo map (f64 precision).
    #[must_use]
    pub fn seconds_to_beat_f64(tempo_map: &mut TempoMap, seconds: f64, fallback_bpm: Bpm) -> f64 {
        tempo_map.ensure_sorted();
        Self::seconds_to_beat_immut(tempo_map, Seconds(seconds), fallback_bpm).0
    }
}

fn default_neg_one() -> Seconds {
    Seconds(-1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::TimelineClip;

    /// 120 BPM for beats 0..4, then 60 BPM.
    fn slowing_map() -> TempoMap {
        let mut map = TempoMap::default();
        map.add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
        map.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);
        map.ensure_sorted();
        map
    }

    #[test]
    fn source_clock_warped_clip_runs_at_its_recorded_tempo() {
        let map = slowing_map();
        let clock = SourceClock::new(&map, Bpm(120.0), None);
        let clip = TimelineClip { recorded_bpm: 100.0, ..Default::default() };
        // 2 beats at 100 BPM = 1.2 s, wherever on the tempo map they sit.
        let secs = clock.source_seconds(&clip, Beats(3.0), Beats(5.0));
        assert!((secs.0 - 1.2).abs() < 1e-9);
        let back = clock.beats_for_source(&clip, Beats(3.0), secs);
        assert!((back.0 - 2.0).abs() < 1e-9);
    }

    #[test]
    fn source_clock_unwarped_clip_follows_the_tempo_map() {
        let map = slowing_map();
        let clock = SourceClock::new(&map, Bpm(120.0), None);
        let clip = TimelineClip::default();
        // Beat 3→4 at 120 BPM (0.5 s) plus 4→5 at 60 BPM (1 s).
        let secs = clock.source_seconds(&clip, Beats(3.0), Beats(5.0));
        // Tolerance covers the tempo map's f32 BPM storage.
        assert!((secs.0 - 1.5).abs() < 1e-6, "3→5 = {} s", secs.0);
        let back = clock.beats_for_source(&clip, Beats(3.0), secs);
        assert!((back.0 - 2.0).abs() < 1e-6, "back = {} beats", back.0);
        // Signed: going backwards gives negative seconds and beats.
        let rev = clock.source_seconds(&clip, Beats(5.0), Beats(3.0));
        assert!((rev.0 + 1.5).abs() < 1e-6);
        let back = clock.beats_for_source(&clip, Beats(5.0), rev);
        assert!((back.0 + 2.0).abs() < 1e-6);
    }

    #[test]
    fn source_clock_project_recorded_tempo_warps_video_but_never_audio() {
        let map = TempoMap::default();
        let clock = SourceClock::new(&map, Bpm(120.0), Some(Bpm(90.0)));
        let video = TimelineClip { video_clip_id: "v".into(), ..Default::default() };
        let audio = TimelineClip { audio_file_path: "a.wav".into(), ..Default::default() };
        // Video falls back to the recorded 90 BPM: 3 beats = 2 s.
        assert!((clock.source_seconds(&video, Beats(0.0), Beats(3.0)).0 - 2.0).abs() < 1e-9);
        // Audio with no tempo of its own stays unwarped at project 120 BPM.
        assert!((clock.source_seconds(&audio, Beats(0.0), Beats(3.0)).0 - 1.5).abs() < 1e-9);
    }

    #[test]
    fn test_constant_tempo() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);

        let seconds = TempoMapConverter::beat_to_seconds(&mut map, Beats(4.0), Bpm(120.0));
        assert!((seconds.0 - 2.0).abs() < 0.001); // 4 beats at 120bpm = 2 seconds

        let beat = TempoMapConverter::seconds_to_beat(&mut map, Seconds(2.0), Bpm(120.0));
        assert!((beat.0 - 4.0).abs() < 0.001);
    }

    #[test]
    fn test_tempo_change() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
        map.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);

        // First 4 beats at 120bpm = 2 seconds
        // Next 4 beats at 60bpm = 4 seconds
        let seconds = TempoMapConverter::beat_to_seconds(&mut map, Beats(8.0), Bpm(120.0));
        assert!((seconds.0 - 6.0).abs() < 0.001);
    }

    #[test]
    fn tempo_map_snapshots_isolate_mutations_and_preserve_wire_shape() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(Beats(8.0), Bpm(180.0), TempoPointSource::Manual, 0.001);
        map.add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);

        let mut sorted_snapshot = map.clone();
        assert!(map.shares_points(&sorted_snapshot));
        sorted_snapshot.ensure_sorted();
        assert!(!map.shares_points(&sorted_snapshot));
        assert_eq!(
            map.points()[0].beat,
            BeatQuantizer::quantize_beat(Beats(8.0))
        );
        assert_eq!(sorted_snapshot.points()[0].beat, Beats::ZERO);

        let mut added = map.clone();
        added.add_or_replace_point(Beats(4.0), Bpm(90.0), TempoPointSource::Manual, 0.001);
        assert!(!map.shares_points(&added));
        assert_eq!(map.point_count(), 2);
        assert_eq!(added.point_count(), 3);

        let mut cleared = map.clone();
        cleared.clear();
        assert!(!map.shares_points(&cleared));
        assert_eq!(map.point_count(), 2);
        assert!(cleared.points().is_empty());

        // Bpm deserialization already clamps input. Inject the invalid value
        // directly to exercise copy-on-write sanitization itself.
        let mut invalid = TempoMap::default();
        Arc::make_mut(&mut invalid.points).push(TempoPoint {
            beat: Beats::ZERO,
            bpm: Bpm(500.0),
            source: TempoPointSource::Manual,
            recorded_at_seconds: Seconds(-1.0),
        });
        let valid_snapshot = invalid.clone();
        invalid.ensure_valid();
        assert!(!invalid.shares_points(&valid_snapshot));
        assert_eq!(valid_snapshot.points()[0].bpm, Bpm(500.0));
        assert_eq!(invalid.points()[0].bpm, Bpm(300.0));

        let wire = serde_json::to_value(&invalid).unwrap();
        assert!(wire.get("points").unwrap().is_array());
        assert!(wire.get("isSorted").is_none());
        let roundtrip: TempoMap = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), wire);
    }

    #[test]
    fn sorting_trivial_and_already_sorted_snapshots_keeps_shared_storage() {
        let mut map = TempoMap::default();
        let mut snapshot = map.clone();
        snapshot.ensure_sorted();
        assert!(snapshot.shares_points(&map));
        map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        let mut snapshot = map.clone();
        snapshot.ensure_sorted();
        assert!(snapshot.shares_points(&map));
        map.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);
        map.ensure_sorted();
        let mut snapshot = map.clone();
        snapshot.ensure_sorted();
        assert!(snapshot.shares_points(&map));
    }

    #[test]
    fn immutable_and_mutable_converters_agree_at_long_nonintegral_positions() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(Beats::ZERO, Bpm(137.37), TempoPointSource::Manual, 0.001);
        map.add_or_replace_point(
            Beats(10_000.0),
            Bpm(211.23),
            TempoPointSource::Manual,
            0.001,
        );

        let beat = Beats(1_000_000.25);
        map.ensure_sorted();
        let immutable_map = map.clone();
        let immutable_seconds =
            TempoMapConverter::beat_to_seconds_immut(&immutable_map, beat, Bpm(120.0));
        let mutable_seconds = TempoMapConverter::beat_to_seconds(&mut map, beat, Bpm(120.0));
        assert_eq!(immutable_seconds, mutable_seconds);

        let immutable_beat =
            TempoMapConverter::seconds_to_beat_immut(&immutable_map, immutable_seconds, Bpm(120.0));
        let mutable_beat =
            TempoMapConverter::seconds_to_beat(&mut map, immutable_seconds, Bpm(120.0));
        assert_eq!(immutable_beat, mutable_beat);
        assert!((immutable_beat.0 - beat.0).abs() < 1e-7);
        assert_eq!(
            TempoMapConverter::seconds_to_beat_f64(&mut map, immutable_seconds.0, Bpm(120.0)),
            immutable_beat.0
        );
    }

    #[test]
    fn converters_handle_boundaries_negative_positions_and_unsorted_wrappers() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);
        map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        let boundary_beat = map.points()[0].beat;
        let boundary_time = Seconds(boundary_beat.0 * 0.5);

        assert_eq!(
            TempoMapConverter::beat_to_seconds(&mut map, boundary_beat, Bpm(120.0)),
            boundary_time
        );
        assert_eq!(
            TempoMapConverter::seconds_to_beat(&mut map, boundary_time, Bpm(120.0)),
            boundary_beat
        );
        assert_eq!(
            TempoMapConverter::beat_to_seconds_immut(&map, Beats(-2.0), Bpm(120.0)),
            Seconds(-1.0)
        );
        assert_eq!(
            TempoMapConverter::seconds_to_beat_immut(&map, Seconds(-1.0), Bpm(120.0)),
            Beats(-2.0)
        );

        let mut unsorted = TempoMap::default();
        unsorted.add_or_replace_point(Beats(8.0), Bpm(180.0), TempoPointSource::Manual, 0.001);
        unsorted.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        unsorted.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);
        let seconds = TempoMapConverter::beat_to_seconds(&mut unsorted, Beats(10.0), Bpm(120.0));
        let b4 = unsorted.points()[1].beat.0;
        let b8 = unsorted.points()[2].beat.0;
        let expected = b4 * 0.5 + (b8 - b4) + (10.0 - b8) / 3.0;
        assert!((seconds.0 - expected).abs() < 1e-12);
    }

    #[test]
    fn test_get_bpm_at_beat_immut_matches_mut() {
        let fallback = Bpm(140.0);

        // Empty map: both should return fallback
        let mut empty_map = TempoMap::default();
        assert_eq!(
            empty_map.get_bpm_at_beat(Beats(4.0), fallback),
            empty_map.get_bpm_at_beat_immut(Beats(4.0), fallback)
        );

        // Test with genuinely unsorted map (built via add_or_replace_point in non-monotonic order)
        let mut unsorted_map = TempoMap::default();
        // Add points in non-monotonic beat order: 8, then 0, then 4
        // This leaves is_sorted = false, so &mut version will sort on first call
        unsorted_map.add_or_replace_point(Beats(8.0), Bpm(180.0), TempoPointSource::Manual, 0.001);
        unsorted_map.add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
        unsorted_map.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);

        // Call IMMUT version FIRST on the fresh unsorted map to catch order bugs
        // Query beat 10 should return 180 (max beat <= 10 is beat 8)
        let immut_result_first = unsorted_map.get_bpm_at_beat_immut(Beats(10.0), fallback);
        assert_eq!(
            immut_result_first,
            Bpm(180.0),
            "Immut version should return max-beat point (180 at beat 8) for query 10"
        );

        // Query before every point should return earliest point's BPM (120 at beat 0)
        let immut_result_before_all = unsorted_map.get_bpm_at_beat_immut(Beats(-1.0), fallback);
        assert_eq!(
            immut_result_before_all,
            Bpm(120.0),
            "Immut version should return earliest point's BPM for query before all points"
        );

        // Now test parity across multiple query beats
        for query_beat in [
            Beats(0.0),
            Beats(2.0),
            Beats(4.0),
            Beats(6.0),
            Beats(8.0),
            Beats(10.0),
        ] {
            let mut_result = unsorted_map.get_bpm_at_beat(query_beat, fallback);
            let immut_result = unsorted_map.get_bpm_at_beat_immut(query_beat, fallback);
            assert_eq!(
                mut_result, immut_result,
                "Mismatch at beat {:?} after &mut sorted",
                query_beat
            );
        }

        // Test with sorted map for completeness
        let mut sorted_map = TempoMap::default();
        sorted_map.add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
        sorted_map.add_or_replace_point(Beats(4.0), Bpm(60.0), TempoPointSource::Manual, 0.001);
        sorted_map.add_or_replace_point(Beats(8.0), Bpm(180.0), TempoPointSource::Manual, 0.001);

        for query_beat in [
            Beats(0.0),
            Beats(2.0),
            Beats(4.0),
            Beats(6.0),
            Beats(8.0),
            Beats(10.0),
        ] {
            let mut_result = sorted_map.get_bpm_at_beat(query_beat, fallback);
            let immut_result = sorted_map.get_bpm_at_beat_immut(query_beat, fallback);
            assert_eq!(
                mut_result, immut_result,
                "Sorted map mismatch at beat {:?}",
                query_beat
            );
        }
    }
}
