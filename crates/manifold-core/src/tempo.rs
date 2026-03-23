use serde::{Deserialize, Serialize};
use crate::types::TempoPointSource;
use crate::math::BeatQuantizer;

/// A single tempo change point in the tempo map.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempoPoint {
    pub beat: f32,
    pub bpm: f32,
    #[serde(default)]
    pub source: TempoPointSource,
    #[serde(default = "default_neg_one")]
    pub recorded_at_seconds: f32,
}

/// Beat-anchored tempo automation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TempoMap {
    #[serde(default)]
    points: Vec<TempoPoint>,

    #[serde(skip)]
    is_sorted: bool,
}

impl TempoMap {
    pub fn ensure_sorted(&mut self) {
        if !self.is_sorted {
            self.points.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap_or(std::cmp::Ordering::Equal));
            self.is_sorted = true;
        }
    }

    /// Validate and sanitize all tempo points.
    pub fn ensure_valid(&mut self) {
        // Remove any points with NaN or infinite BPM/beat
        self.points.retain(|p| p.bpm.is_finite() && p.beat.is_finite());
        // Clamp BPM to 20-300
        for p in &mut self.points {
            p.bpm = p.bpm.clamp(20.0, 300.0);
        }
        // Re-sort by beat
        self.points.sort_by(|a, b| a.beat.partial_cmp(&b.beat).unwrap_or(std::cmp::Ordering::Equal));
        self.is_sorted = true;
    }

    /// Get BPM at a given beat (step-change lookup).
    /// Unity TempoMap.cs lines 198-214: initializes from points[0].bpm, not fallback.
    pub fn get_bpm_at_beat(&mut self, beat: f32, fallback: f32) -> f32 {
        self.ensure_sorted();
        if self.points.is_empty() {
            return fallback.clamp(20.0, 300.0);
        }
        let mut bpm = self.points[0].bpm;
        for point in &self.points {
            if point.beat <= beat {
                bpm = point.bpm;
            } else {
                break;
            }
        }
        bpm.clamp(20.0, 300.0)
    }

    pub fn add_or_replace_point(
        &mut self,
        beat: f32,
        bpm: f32,
        source: TempoPointSource,
        epsilon: f32,
    ) {
        self.add_or_replace_point_with_time(beat, bpm, source, epsilon, -1.0);
    }

    pub fn add_or_replace_point_with_time(
        &mut self,
        beat: f32,
        bpm: f32,
        source: TempoPointSource,
        epsilon: f32,
        recorded_at_seconds: f32,
    ) {
        let beat = BeatQuantizer::quantize_beat(beat);
        let bpm = BeatQuantizer::quantize_bpm(bpm);

        // Remove existing point at same beat (within epsilon)
        self.points.retain(|p| (p.beat - beat).abs() > epsilon);

        self.points.push(TempoPoint {
            beat,
            bpm,
            source,
            recorded_at_seconds,
        });
        self.is_sorted = false;
    }

    pub fn ensure_default_at_beat_zero(&mut self, fallback_bpm: f32, source: TempoPointSource) {
        self.ensure_sorted();
        if self.points.is_empty() || self.points[0].beat > 0.001 {
            self.add_or_replace_point(0.0, fallback_bpm, source, 0.001);
        }
    }

    /// Read-only access to tempo points.
    #[inline]
    pub fn points(&self) -> &[TempoPoint] {
        &self.points
    }

    /// Number of tempo points.
    #[inline]
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Clear all tempo points.
    pub fn clear(&mut self) {
        self.points.clear();
        self.is_sorted = true;
    }

    /// Clone all tempo points.
    pub fn clone_points(&self) -> Vec<TempoPoint> {
        self.points.clone()
    }

    /// Get sorted reference to points.
    pub fn get_sorted_points(&mut self) -> &[TempoPoint] {
        self.ensure_sorted();
        &self.points
    }

    /// Replace all points.
    pub fn set_points(&mut self, points: Vec<TempoPoint>) {
        self.points = points;
        self.is_sorted = false;
    }
}

/// Pure tempo math — beat↔seconds conversion via piecewise integration.
/// Port of Unity TempoMapConverter.cs.
pub struct TempoMapConverter;

impl TempoMapConverter {
    /// Unity TempoMapConverter.cs line 111-113: clamps BPM to 20-300.
    pub fn seconds_per_beat_from_bpm(bpm: f32) -> f32 {
        60.0 / bpm.clamp(20.0, 300.0)
    }

    /// Get BPM at beat 0 from tempo map, with fallback.
    /// Unity TempoMapConverter.cs lines 116-121.
    fn get_bpm_at_beat_zero(tempo_map: &mut TempoMap, fallback_bpm: f32) -> f32 {
        if tempo_map.points.is_empty() {
            return fallback_bpm.clamp(20.0, 300.0);
        }
        tempo_map.get_bpm_at_beat(0.0, fallback_bpm)
    }

    /// Convert beat position to seconds using tempo map.
    /// Unity TempoMapConverter.cs lines 14-56.
    pub fn beat_to_seconds(tempo_map: &mut TempoMap, beat: f32, fallback_bpm: f32) -> f32 {
        tempo_map.ensure_sorted();
        let bpm_at_zero = Self::get_bpm_at_beat_zero(tempo_map, fallback_bpm);
        let spb_at_zero = Self::seconds_per_beat_from_bpm(bpm_at_zero);

        if tempo_map.points.is_empty() {
            return beat * spb_at_zero;
        }

        // Negative-beat conversion uses beat-0 tempo
        if beat <= 0.0 {
            return beat * spb_at_zero;
        }

        let mut total_seconds = 0.0f32;
        let mut current_beat = 0.0f32;
        let mut current_bpm = bpm_at_zero;

        for point in &tempo_map.points {
            // Skip points at or before beat 0 (absorb their BPM)
            if point.beat <= 0.0 {
                current_bpm = point.bpm;
                continue;
            }

            if point.beat >= beat {
                break;
            }

            let delta_beats = point.beat - current_beat;
            if delta_beats > 0.0 {
                total_seconds += delta_beats * Self::seconds_per_beat_from_bpm(current_bpm);
            }

            current_beat = point.beat;
            current_bpm = point.bpm;
        }

        let tail_beats = beat - current_beat;
        if tail_beats > 0.0 {
            total_seconds += tail_beats * Self::seconds_per_beat_from_bpm(current_bpm);
        }

        total_seconds
    }

    /// Immutable version of beat_to_seconds. Assumes tempo map is already sorted
    /// (guaranteed after on_after_deserialize / ensure_valid).
    pub fn beat_to_seconds_immut(tempo_map: &TempoMap, beat: f32, fallback_bpm: f32) -> f32 {
        let bpm_at_zero = if tempo_map.points.is_empty() {
            fallback_bpm.clamp(20.0, 300.0)
        } else {
            // Inline get_bpm_at_beat logic for immutable access
            let mut bpm = tempo_map.points[0].bpm;
            for point in &tempo_map.points {
                if point.beat <= 0.0 {
                    bpm = point.bpm;
                } else {
                    break;
                }
            }
            bpm.clamp(20.0, 300.0)
        };
        let spb_at_zero = Self::seconds_per_beat_from_bpm(bpm_at_zero);

        if tempo_map.points.is_empty() {
            return beat * spb_at_zero;
        }

        if beat <= 0.0 {
            return beat * spb_at_zero;
        }

        let mut total_seconds = 0.0f32;
        let mut current_beat = 0.0f32;
        let mut current_bpm = bpm_at_zero;

        for point in &tempo_map.points {
            if point.beat <= 0.0 {
                current_bpm = point.bpm;
                continue;
            }
            if point.beat >= beat {
                break;
            }
            let delta_beats = point.beat - current_beat;
            if delta_beats > 0.0 {
                total_seconds += delta_beats * Self::seconds_per_beat_from_bpm(current_bpm);
            }
            current_beat = point.beat;
            current_bpm = point.bpm;
        }

        let tail_beats = beat - current_beat;
        if tail_beats > 0.0 {
            total_seconds += tail_beats * Self::seconds_per_beat_from_bpm(current_bpm);
        }

        total_seconds
    }

    /// Convert seconds to beat position using tempo map.
    /// Unity TempoMapConverter.cs lines 61-109.
    pub fn seconds_to_beat(tempo_map: &mut TempoMap, seconds: f32, fallback_bpm: f32) -> f32 {
        tempo_map.ensure_sorted();
        let bpm_at_zero = Self::get_bpm_at_beat_zero(tempo_map, fallback_bpm);
        let spb_at_zero = Self::seconds_per_beat_from_bpm(bpm_at_zero);

        if tempo_map.points.is_empty() {
            return if spb_at_zero > 0.0 { seconds / spb_at_zero } else { 0.0 };
        }

        // Negative-time conversion uses beat-0 tempo
        if seconds <= 0.0 {
            return if spb_at_zero > 0.0 { seconds / spb_at_zero } else { 0.0 };
        }

        let mut remaining_seconds = seconds;
        let mut current_beat = 0.0f32;
        let mut current_bpm = bpm_at_zero;

        for point in &tempo_map.points {
            // Skip points at or before beat 0 (absorb their BPM)
            if point.beat <= 0.0 {
                current_bpm = point.bpm;
                continue;
            }

            let segment_beats = point.beat - current_beat;
            if segment_beats <= 0.0 {
                current_beat = point.beat;
                current_bpm = point.bpm;
                continue;
            }

            let segment_seconds = segment_beats * Self::seconds_per_beat_from_bpm(current_bpm);
            if remaining_seconds <= segment_seconds {
                let spb = Self::seconds_per_beat_from_bpm(current_bpm);
                return if spb > 0.0 { current_beat + remaining_seconds / spb } else { current_beat };
            }

            remaining_seconds -= segment_seconds;
            current_beat = point.beat;
            current_bpm = point.bpm;
        }

        let tail_spb = Self::seconds_per_beat_from_bpm(current_bpm);
        if tail_spb > 0.0 { current_beat + remaining_seconds / tail_spb } else { current_beat }
    }
}

fn default_neg_one() -> f32 { -1.0 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constant_tempo() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(0.0, 120.0, TempoPointSource::Manual, 0.001);

        let seconds = TempoMapConverter::beat_to_seconds(&mut map, 4.0, 120.0);
        assert!((seconds - 2.0).abs() < 0.001); // 4 beats at 120bpm = 2 seconds

        let beat = TempoMapConverter::seconds_to_beat(&mut map, 2.0, 120.0);
        assert!((beat - 4.0).abs() < 0.001);
    }

    #[test]
    fn test_tempo_change() {
        let mut map = TempoMap::default();
        map.add_or_replace_point(0.0, 120.0, TempoPointSource::Manual, 0.001);
        map.add_or_replace_point(4.0, 60.0, TempoPointSource::Manual, 0.001);

        // First 4 beats at 120bpm = 2 seconds
        // Next 4 beats at 60bpm = 4 seconds
        let seconds = TempoMapConverter::beat_to_seconds(&mut map, 8.0, 120.0);
        assert!((seconds - 6.0).abs() < 0.001);
    }
}
