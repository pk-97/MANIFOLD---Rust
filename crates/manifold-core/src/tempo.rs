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
    pub points: Vec<TempoPoint>,

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
    pub fn get_bpm_at_beat(&mut self, beat: f32, fallback: f32) -> f32 {
        self.ensure_sorted();
        let mut bpm = fallback;
        for point in &self.points {
            if point.beat <= beat {
                bpm = point.bpm;
            } else {
                break;
            }
        }
        bpm.clamp(20.0, 300.0)
    }

    pub fn add_or_replace_point(&mut self, beat: f32, bpm: f32, source: TempoPointSource, epsilon: f32) {
        let beat = BeatQuantizer::quantize_beat(beat);
        let bpm = BeatQuantizer::quantize_bpm(bpm);

        // Remove existing point at same beat (within epsilon)
        self.points.retain(|p| (p.beat - beat).abs() > epsilon);

        self.points.push(TempoPoint {
            beat,
            bpm,
            source,
            recorded_at_seconds: -1.0,
        });
        self.is_sorted = false;
    }

    pub fn ensure_default_at_beat_zero(&mut self, fallback_bpm: f32, source: TempoPointSource) {
        self.ensure_sorted();
        if self.points.is_empty() || self.points[0].beat > 0.001 {
            self.add_or_replace_point(0.0, fallback_bpm, source, 0.001);
        }
    }

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
pub struct TempoMapConverter;

impl TempoMapConverter {
    pub fn seconds_per_beat_from_bpm(bpm: f32) -> f32 {
        if bpm <= 0.0 { return 0.5; }
        60.0 / bpm
    }

    /// Convert beat position to seconds using tempo map.
    pub fn beat_to_seconds(tempo_map: &mut TempoMap, beat: f32, fallback_bpm: f32) -> f32 {
        tempo_map.ensure_sorted();
        let points = &tempo_map.points;

        if points.is_empty() {
            return beat * Self::seconds_per_beat_from_bpm(fallback_bpm);
        }

        let mut seconds = 0.0f32;
        let mut prev_beat = 0.0f32;
        let mut current_bpm = fallback_bpm;

        for point in points {
            if point.beat >= beat {
                break;
            }
            // Accumulate time from prev_beat to this point
            let segment_beats = point.beat - prev_beat;
            if segment_beats > 0.0 {
                seconds += segment_beats * Self::seconds_per_beat_from_bpm(current_bpm);
            }
            current_bpm = point.bpm;
            prev_beat = point.beat;
        }

        // Final segment from last point to target beat
        let remaining = beat - prev_beat;
        if remaining > 0.0 {
            seconds += remaining * Self::seconds_per_beat_from_bpm(current_bpm);
        }

        seconds
    }

    /// Convert seconds to beat position using tempo map.
    pub fn seconds_to_beat(tempo_map: &mut TempoMap, seconds: f32, fallback_bpm: f32) -> f32 {
        tempo_map.ensure_sorted();
        let points = &tempo_map.points;

        if points.is_empty() {
            let spb = Self::seconds_per_beat_from_bpm(fallback_bpm);
            return if spb > 0.0 { seconds / spb } else { 0.0 };
        }

        let mut accumulated_seconds = 0.0f32;
        let mut prev_beat = 0.0f32;
        let mut current_bpm = fallback_bpm;

        for point in points {
            let segment_beats = point.beat - prev_beat;
            let segment_seconds = segment_beats * Self::seconds_per_beat_from_bpm(current_bpm);

            if accumulated_seconds + segment_seconds >= seconds {
                // Target is within this segment
                let remaining_seconds = seconds - accumulated_seconds;
                let spb = Self::seconds_per_beat_from_bpm(current_bpm);
                return prev_beat + if spb > 0.0 { remaining_seconds / spb } else { 0.0 };
            }

            accumulated_seconds += segment_seconds;
            current_bpm = point.bpm;
            prev_beat = point.beat;
        }

        // Past last point
        let remaining_seconds = seconds - accumulated_seconds;
        let spb = Self::seconds_per_beat_from_bpm(current_bpm);
        prev_beat + if spb > 0.0 { remaining_seconds / spb } else { 0.0 }
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
