// Port of Unity PercussionAnalysisModels.cs (561 lines) + BeatTimeConverter.cs (90 lines).
// All data types for the percussion analysis pipeline.

use serde::{Deserialize, Serialize};
use serde::de::Deserializer;
use serde::ser::Serializer;

use crate::percussion::ImportedPercussionClipPlacement;
use crate::project::Project;
use crate::tempo::TempoMapConverter;
use crate::types::GeneratorType;

// ─── PercussionTriggerType ───

/// Port of Unity PercussionTriggerType enum.
/// Normalized trigger classes used by percussion detection and clip mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum PercussionTriggerType {
    #[default]
    Unknown = 0,
    Kick = 1,
    Snare = 2,
    Clap = 3,
    Hat = 4,
    Perc = 5,
    Bass = 6,
    Synth = 7,
    Pad = 8,
    Vocal = 9,
    BassSustained = 10,
}

impl Serialize for PercussionTriggerType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for PercussionTriggerType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match &value {
            serde_json::Value::Number(n) => match n.as_i64().unwrap_or(0) as i32 {
                0 => Self::Unknown,
                1 => Self::Kick,
                2 => Self::Snare,
                3 => Self::Clap,
                4 => Self::Hat,
                5 => Self::Perc,
                6 => Self::Bass,
                7 => Self::Synth,
                8 => Self::Pad,
                9 => Self::Vocal,
                10 => Self::BassSustained,
                _ => Self::Unknown,
            },
            _ => Self::Unknown,
        })
    }
}

// ─── PercussionEvent ───

/// Port of Unity PercussionEvent class.
/// Single detected percussive event in seconds-domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionEvent {
    pub trigger_type: PercussionTriggerType,
    pub time_seconds: f32,
    pub confidence: f32,
    pub duration_seconds: f32,
}

impl PercussionEvent {
    pub fn new(
        trigger_type: PercussionTriggerType,
        time_seconds: f32,
        confidence: f32,
        duration_seconds: f32,
    ) -> Self {
        Self {
            trigger_type,
            time_seconds: time_seconds.max(0.0),
            confidence: confidence.clamp(0.0, 1.0),
            duration_seconds: duration_seconds.max(0.0),
        }
    }

    pub fn has_duration(&self) -> bool {
        self.duration_seconds > 0.0
    }
}

// ─── PercussionBeatGrid ───

/// Port of Unity PercussionBeatGrid class.
/// Beat-grid metadata emitted by analysis for seconds-to-beat mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionBeatGrid {
    pub mode: String,
    pub beat_times_seconds: Vec<f32>,
    pub downbeat_indices: Vec<i32>,
    pub bpm_derived: f32,
    pub confidence: f32,
    pub onset_to_peak_seconds: f32,
}

impl PercussionBeatGrid {
    pub fn new(
        mode: &str,
        beat_times_seconds: Vec<f32>,
        downbeat_indices: Vec<i32>,
        bpm_derived: f32,
        confidence: f32,
        onset_to_peak_seconds: f32,
    ) -> Self {
        let mode = if mode.trim().is_empty() {
            "beat_times".to_string()
        } else {
            mode.to_string()
        };
        let bpm_derived = if bpm_derived > 0.0 {
            bpm_derived.clamp(20.0, 300.0)
        } else {
            0.0
        };
        let confidence = confidence.clamp(0.0, 1.0);
        let onset_to_peak_seconds = onset_to_peak_seconds.clamp(0.0, 0.050);

        let mut grid = Self {
            mode,
            beat_times_seconds,
            downbeat_indices,
            bpm_derived,
            confidence,
            onset_to_peak_seconds,
        };
        grid.ensure_valid();
        grid
    }

    /// Port of Unity PercussionBeatGrid.BpmDerived property getter.
    pub fn bpm_derived_clamped(&self) -> f32 {
        if self.bpm_derived > 0.0 {
            self.bpm_derived.clamp(20.0, 300.0)
        } else {
            0.0
        }
    }

    /// Port of Unity PercussionBeatGrid.Confidence property getter.
    pub fn confidence_clamped(&self) -> f32 {
        self.confidence.clamp(0.0, 1.0)
    }

    /// Port of Unity PercussionBeatGrid.OnsetToPeakSeconds property getter.
    pub fn onset_to_peak_seconds_clamped(&self) -> f32 {
        self.onset_to_peak_seconds.clamp(0.0, 0.050)
    }

    pub fn has_usable_beats(&self) -> bool {
        self.beat_times_seconds.len() >= 2
    }

    /// Port of Unity PercussionBeatGrid.EnsureValid().
    pub fn ensure_valid(&mut self) {
        if self.mode.trim().is_empty() {
            self.mode = "beat_times".to_string();
        }

        // Remove non-finite or negative beat times.
        self.beat_times_seconds
            .retain(|&t| t.is_finite() && t >= 0.0);
        self.beat_times_seconds.sort_by(|a, b| a.partial_cmp(b).unwrap());

        // Deduplicate near-identical beat markers to keep interpolation stable.
        let mut i = self.beat_times_seconds.len().saturating_sub(1);
        while i >= 1 {
            if (self.beat_times_seconds[i] - self.beat_times_seconds[i - 1]).abs() < 0.0001 {
                self.beat_times_seconds.remove(i);
            }
            i = i.saturating_sub(1);
        }

        let beat_count = self.beat_times_seconds.len() as i32;
        // Remove out-of-range downbeat indices.
        self.downbeat_indices
            .retain(|&idx| idx >= 0 && idx < beat_count);
        self.downbeat_indices.sort();
        // Deduplicate downbeat indices.
        let mut i = self.downbeat_indices.len().saturating_sub(1);
        while i >= 1 {
            if self.downbeat_indices[i] == self.downbeat_indices[i - 1] {
                self.downbeat_indices.remove(i);
            }
            i = i.saturating_sub(1);
        }

        if !self.bpm_derived.is_finite() || self.bpm_derived <= 0.0 {
            self.bpm_derived = 0.0;
        } else {
            self.bpm_derived = self.bpm_derived.clamp(20.0, 300.0);
        }

        if !self.confidence.is_finite() {
            self.confidence = 0.0;
        }
        self.confidence = self.confidence.clamp(0.0, 1.0);
    }

    /// Port of Unity PercussionBeatGrid.TryMapSecondsToBeat().
    pub fn try_map_seconds_to_beat(&self, seconds: f32) -> Option<f32> {
        if !self.has_usable_beats() || !seconds.is_finite() {
            return None;
        }

        let interval = self.try_get_representative_interval()?;
        if interval <= 0.000001 {
            return None;
        }

        let first_beat_seconds = self.beat_times_seconds[0];
        let beat_offset = if first_beat_seconds > (interval * 0.75) {
            first_beat_seconds / interval
        } else {
            0.0
        };

        let target_seconds = seconds.max(0.0);
        let last_index = self.beat_times_seconds.len() - 1;

        if target_seconds <= first_beat_seconds {
            let beat = beat_offset + ((target_seconds - first_beat_seconds) / interval);
            return if beat.is_finite() { Some(beat) } else { None };
        }

        if target_seconds >= self.beat_times_seconds[last_index] {
            let beat = beat_offset
                + last_index as f32
                + ((target_seconds - self.beat_times_seconds[last_index]) / interval);
            return if beat.is_finite() { Some(beat) } else { None };
        }

        // Binary search for the containing segment.
        let mut lo = 0usize;
        let mut hi = last_index;
        while hi - lo > 1 {
            let mid = lo + ((hi - lo) / 2);
            if self.beat_times_seconds[mid] <= target_seconds {
                lo = mid;
            } else {
                hi = mid;
            }
        }

        let start = self.beat_times_seconds[lo];
        let end = self.beat_times_seconds[lo + 1];
        let segment = end - start;
        if !segment.is_finite() || segment <= 0.000001 {
            return None;
        }

        let t = (target_seconds - start) / segment;
        let beat = beat_offset + lo as f32 + t;
        if beat.is_finite() {
            Some(beat)
        } else {
            None
        }
    }

    /// Port of Unity PercussionBeatGrid.TryGetRepresentativeInterval().
    fn try_get_representative_interval(&self) -> Option<f32> {
        if self.beat_times_seconds.len() < 2 {
            return None;
        }

        // Middle interval is robust enough here and avoids sorting/allocating.
        let mid = self.beat_times_seconds.len() / 2;
        let i0 = (mid.saturating_sub(1)).min(self.beat_times_seconds.len() - 2);
        let i1 = i0 + 1;
        let candidate = self.beat_times_seconds[i1] - self.beat_times_seconds[i0];
        if candidate.is_finite() && candidate > 0.000001 {
            return Some(candidate);
        }

        // Fallback scan when central segment is degenerate.
        for i in 0..self.beat_times_seconds.len() - 1 {
            let d = self.beat_times_seconds[i + 1] - self.beat_times_seconds[i];
            if d.is_finite() && d > 0.000001 {
                return Some(d);
            }
        }

        None
    }
}

// ─── PercussionAnalysisData ───

/// Port of Unity PercussionAnalysisData class.
/// Parsed percussion analysis payload for one track/input segment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionAnalysisData {
    pub track_id: String,
    pub bpm: f32,
    pub bpm_confidence: f32,
    pub beat_grid: Option<PercussionBeatGrid>,
    pub events: Vec<PercussionEvent>,
    pub energy_envelope: Option<Vec<f32>>,
}

impl PercussionAnalysisData {
    pub fn new(
        track_id: &str,
        bpm: f32,
        events: Vec<PercussionEvent>,
        bpm_confidence: f32,
        beat_grid: Option<PercussionBeatGrid>,
        energy_envelope: Option<Vec<f32>>,
    ) -> Self {
        let track_id = if track_id.is_empty() {
            String::new()
        } else {
            track_id.to_string()
        };
        let bpm = if bpm > 0.0 { bpm.clamp(20.0, 300.0) } else { 0.0 };
        let bpm_confidence = if bpm_confidence.is_finite() {
            bpm_confidence.clamp(0.0, 1.0)
        } else {
            0.0
        };

        let mut data = Self {
            track_id,
            bpm,
            bpm_confidence,
            beat_grid,
            events,
            energy_envelope,
        };
        if let Some(ref mut grid) = data.beat_grid {
            grid.ensure_valid();
        }
        data
    }

    pub fn new_simple(track_id: &str, bpm: f32, events: Vec<PercussionEvent>) -> Self {
        Self::new(track_id, bpm, events, 0.0, None, None)
    }

    pub fn has_energy_envelope(&self) -> bool {
        self.energy_envelope
            .as_ref()
            .map_or(false, |e| !e.is_empty())
    }

    /// Port of Unity PercussionAnalysisData.EnergyAtBeat().
    /// Returns normalized energy [0,1] at the given beat via linear interpolation.
    /// Beat 0 = index 0. Returns 1.0 if no envelope data is present.
    pub fn energy_at_beat(&self, beat: f32) -> f32 {
        let envelope = match &self.energy_envelope {
            Some(e) if !e.is_empty() => e,
            _ => return 1.0,
        };
        if beat <= 0.0 {
            return envelope[0];
        }
        let last_idx = envelope.len() - 1;
        if beat >= last_idx as f32 {
            return envelope[last_idx];
        }
        let lo = beat as usize;
        let t = beat - lo as f32;
        envelope[lo] + t * (envelope[lo + 1] - envelope[lo])
    }

    /// Port of Unity PercussionAnalysisData.EnsureValid().
    pub fn ensure_valid(&mut self) {
        if !self.bpm.is_finite() || self.bpm <= 0.0 {
            self.bpm = 0.0;
        } else {
            self.bpm = self.bpm.clamp(20.0, 300.0);
        }

        if !self.bpm_confidence.is_finite() {
            self.bpm_confidence = 0.0;
        }
        self.bpm_confidence = self.bpm_confidence.clamp(0.0, 1.0);

        if let Some(ref mut grid) = self.beat_grid {
            grid.ensure_valid();
        }
        if self.bpm <= 0.0 {
            if let Some(ref grid) = self.beat_grid {
                if grid.bpm_derived_clamped() > 0.0 {
                    self.bpm = grid.bpm_derived_clamped();
                }
            }
        }
        if self.bpm_confidence <= 0.0 {
            if let Some(ref grid) = self.beat_grid {
                self.bpm_confidence = grid.confidence_clamped();
            }
        }

        if let Some(ref mut envelope) = self.energy_envelope {
            for val in envelope.iter_mut() {
                if !val.is_finite() {
                    *val = 1.0;
                } else {
                    *val = val.clamp(0.0, 1.0);
                }
            }
        }

        self.events.retain(|_| true); // no null check needed in Rust
        self.events
            .sort_by(|a, b| a.time_seconds.partial_cmp(&b.time_seconds).unwrap());
    }

    /// Port of Unity PercussionAnalysisData.TryMapSecondsToBeat().
    /// Prefers the project BPM converter for consistency with the reprojection path.
    pub fn try_map_seconds_to_beat(
        &self,
        seconds: f32,
        fallback_converter: Option<&mut dyn BeatTimeConverter>,
    ) -> Option<f32> {
        // Prefer the project BPM converter for consistency with the
        // reprojection path (PercussionClipReprojectionPlanner), which
        // always uses the converter.
        if let Some(converter) = fallback_converter {
            let beat = converter.seconds_to_beat(seconds);
            return if beat.is_finite() { Some(beat) } else { None };
        }

        if let Some(ref grid) = self.beat_grid {
            return grid.try_map_seconds_to_beat(seconds);
        }

        None
    }
}

// ─── PercussionClipBinding ───

/// Port of Unity PercussionClipBinding class.
/// Maps a detected percussion trigger to a target timeline placement template.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionClipBinding {
    pub trigger_type: PercussionTriggerType,
    pub layer_index: i32,
    pub video_clip_id: Option<String>,
    pub generator_type: GeneratorType,
    pub duration_beats: f32,
    pub minimum_confidence: f32,
}

impl PercussionClipBinding {
    pub fn new(
        trigger_type: PercussionTriggerType,
        layer_index: i32,
        video_clip_id: Option<String>,
        generator_type: GeneratorType,
        duration_beats: f32,
        minimum_confidence: f32,
    ) -> Self {
        Self {
            trigger_type,
            layer_index: layer_index.max(0),
            video_clip_id,
            generator_type,
            duration_beats: duration_beats.max(0.0),
            minimum_confidence: minimum_confidence.clamp(0.0, 1.0),
        }
    }

    pub fn uses_generator(&self) -> bool {
        self.generator_type != GeneratorType::None
    }

    /// Port of Unity PercussionClipBinding.WithVideoClipId().
    pub fn with_video_clip_id(&self, resolved_video_clip_id: &str) -> Self {
        Self {
            trigger_type: self.trigger_type,
            layer_index: self.layer_index,
            video_clip_id: Some(resolved_video_clip_id.to_string()),
            generator_type: self.generator_type,
            duration_beats: self.duration_beats,
            minimum_confidence: self.minimum_confidence,
        }
    }

    /// Port of Unity PercussionClipBinding.AsGeneratorBinding().
    pub fn as_generator_binding(&self, resolved_generator_type: GeneratorType) -> Self {
        Self {
            trigger_type: self.trigger_type,
            layer_index: self.layer_index,
            video_clip_id: self.video_clip_id.clone(),
            generator_type: resolved_generator_type,
            duration_beats: self.duration_beats,
            minimum_confidence: self.minimum_confidence,
        }
    }
}

// ─── PercussionImportOptions ───

/// Port of Unity PercussionImportOptions class.
/// Planner configuration controlling quantization, offsets, and fallback behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionImportOptions {
    pub start_beat_offset: f32,
    pub quantize_to_grid: bool,
    pub quantize_step_beats: f32,
    pub default_clip_duration_beats: f32,
    pub onset_compensation_seconds: f32,
    pub minimum_energy_gate: f32,
    pub bindings: Vec<PercussionClipBinding>,
}

impl Default for PercussionImportOptions {
    fn default() -> Self {
        Self {
            start_beat_offset: 0.0,
            quantize_to_grid: true,
            quantize_step_beats: 0.25,
            default_clip_duration_beats: 0.25,
            onset_compensation_seconds: 0.0,
            minimum_energy_gate: 0.0,
            bindings: Vec::new(),
        }
    }
}

// ─── PercussionClipPlacement ───

/// Port of Unity PercussionClipPlacement class.
/// Planned clip placement in beat-domain, resolved from one percussion event.
#[derive(Debug, Clone)]
pub struct PercussionClipPlacement {
    pub trigger_type: PercussionTriggerType,
    pub layer_index: i32,
    pub video_clip_id: Option<String>,
    pub generator_type: GeneratorType,
    pub start_beat: f32,
    pub duration_beats: f32,
    pub confidence: f32,
    pub source_time_seconds: f32,
}

impl PercussionClipPlacement {
    pub fn new(
        trigger_type: PercussionTriggerType,
        layer_index: i32,
        video_clip_id: Option<String>,
        generator_type: GeneratorType,
        start_beat: f32,
        duration_beats: f32,
        confidence: f32,
        source_time_seconds: f32,
    ) -> Self {
        Self {
            trigger_type,
            layer_index: layer_index.max(0),
            video_clip_id,
            generator_type,
            start_beat: start_beat.max(0.0),
            duration_beats: duration_beats.max(0.0),
            confidence: confidence.clamp(0.0, 1.0),
            source_time_seconds: source_time_seconds.max(0.0),
        }
    }

    pub fn is_generator(&self) -> bool {
        self.generator_type != GeneratorType::None
    }
}

// ─── PercussionPlacementPlan ───

/// Port of Unity PercussionPlacementPlan class.
/// Planner output with placement list and skip counters for diagnostics.
#[derive(Debug, Clone, Default)]
pub struct PercussionPlacementPlan {
    placements: Vec<PercussionClipPlacement>,
    pub total_events: i32,
    pub skipped_unknown_type: i32,
    pub skipped_invalid_timing: i32,
    pub skipped_unmapped: i32,
    pub skipped_low_confidence: i32,
    pub skipped_by_quantized_dedup: i32,
    pub skipped_low_energy: i32,
}

impl PercussionPlacementPlan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn accepted_events(&self) -> usize {
        self.placements.len()
    }

    pub fn placements(&self) -> &[PercussionClipPlacement] {
        &self.placements
    }

    pub fn add_placement(&mut self, placement: PercussionClipPlacement) {
        self.placements.push(placement);
    }

    pub fn sort_placements(&mut self) {
        self.placements
            .sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());
    }
}

// ─── BeatTimeConverter trait ───

/// Port of Unity IBeatTimeConverter interface.
pub trait BeatTimeConverter {
    fn seconds_to_beat(&mut self, seconds: f32) -> f32;
}

/// Port of Unity ProjectBeatTimeConverter class.
/// Converts seconds-domain events into timeline beats using the project's tempo map.
pub struct ProjectBeatTimeConverter<'a> {
    project: &'a mut Project,
}

impl<'a> ProjectBeatTimeConverter<'a> {
    pub fn new(project: &'a mut Project) -> Self {
        Self { project }
    }
}

impl<'a> BeatTimeConverter for ProjectBeatTimeConverter<'a> {
    fn seconds_to_beat(&mut self, seconds: f32) -> f32 {
        let fallback_bpm = self.project.settings.bpm;
        TempoMapConverter::seconds_to_beat(
            &mut self.project.tempo_map,
            seconds,
            fallback_bpm,
        )
    }
}

// ─── PercussionClipReprojectionPlanner ───

/// Port of Unity PercussionClipReprojectionPlanner static class.
/// Deterministic mapping utility for reprojection of imported percussion clips
/// after tempo/grid changes.
pub struct PercussionClipReprojectionPlanner;

impl PercussionClipReprojectionPlanner {
    /// Port of Unity TryComputeAlignedSourceBeat().
    pub fn try_compute_aligned_source_beat(
        placement: &ImportedPercussionClipPlacement,
        source_time_seconds: f32,
        beat_time_converter: &mut dyn BeatTimeConverter,
    ) -> Option<f32> {
        if placement.clip_id.is_empty() {
            return None;
        }

        let seconds = source_time_seconds.max(0.0);
        let mut source_beat =
            beat_time_converter.seconds_to_beat(seconds) + placement.start_beat_offset;
        if !source_beat.is_finite() {
            return None;
        }

        source_beat += placement.alignment_offset_beats;

        let slope = placement.alignment_slope_beats_per_second;
        if slope != 0.0 {
            let pivot = placement.alignment_pivot_seconds;
            source_beat += slope * (seconds - pivot);
        }

        source_beat = source_beat.max(0.0);
        if source_beat.is_finite() {
            Some(source_beat)
        } else {
            None
        }
    }

    /// Port of Unity TryComputePlacementBeat().
    pub fn try_compute_placement_beat(
        placement: &ImportedPercussionClipPlacement,
        beat_time_converter: &mut dyn BeatTimeConverter,
    ) -> Option<(f32, f32)> {
        let source_beat = Self::try_compute_aligned_source_beat(
            placement,
            placement.source_time_seconds,
            beat_time_converter,
        )?;

        let mut placement_beat = source_beat;
        if placement.quantize_to_grid && placement.quantize_step_beats > 0.0 {
            placement_beat =
                (source_beat / placement.quantize_step_beats).round() * placement.quantize_step_beats;
        }
        placement_beat = placement_beat.max(0.0);

        if source_beat.is_finite() && placement_beat.is_finite() {
            Some((source_beat, placement_beat))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_type_discriminants() {
        assert_eq!(PercussionTriggerType::Unknown as i32, 0);
        assert_eq!(PercussionTriggerType::Kick as i32, 1);
        assert_eq!(PercussionTriggerType::Snare as i32, 2);
        assert_eq!(PercussionTriggerType::Clap as i32, 3);
        assert_eq!(PercussionTriggerType::Hat as i32, 4);
        assert_eq!(PercussionTriggerType::Perc as i32, 5);
        assert_eq!(PercussionTriggerType::Bass as i32, 6);
        assert_eq!(PercussionTriggerType::Synth as i32, 7);
        assert_eq!(PercussionTriggerType::Pad as i32, 8);
        assert_eq!(PercussionTriggerType::Vocal as i32, 9);
        assert_eq!(PercussionTriggerType::BassSustained as i32, 10);
    }

    #[test]
    fn test_percussion_event_clamping() {
        let e = PercussionEvent::new(PercussionTriggerType::Kick, -1.0, 2.0, -0.5);
        assert_eq!(e.time_seconds, 0.0);
        assert_eq!(e.confidence, 1.0);
        assert_eq!(e.duration_seconds, 0.0);
    }

    #[test]
    fn test_beat_grid_ensure_valid_removes_negative() {
        let mut grid = PercussionBeatGrid {
            mode: String::new(),
            beat_times_seconds: vec![-1.0, 0.5, 1.0, f32::NAN],
            downbeat_indices: vec![-1, 0, 5],
            bpm_derived: 120.0,
            confidence: 0.8,
            onset_to_peak_seconds: 0.0,
        };
        grid.ensure_valid();
        assert_eq!(grid.mode, "beat_times");
        assert_eq!(grid.beat_times_seconds, vec![0.5, 1.0]);
        assert_eq!(grid.downbeat_indices, vec![0]);
    }

    #[test]
    fn test_beat_grid_dedup() {
        let mut grid = PercussionBeatGrid {
            mode: "beat_times".to_string(),
            beat_times_seconds: vec![1.0, 1.00005, 2.0],
            downbeat_indices: vec![],
            bpm_derived: 120.0,
            confidence: 0.8,
            onset_to_peak_seconds: 0.0,
        };
        grid.ensure_valid();
        assert_eq!(grid.beat_times_seconds.len(), 2);
    }

    #[test]
    fn test_beat_grid_map_seconds_to_beat() {
        let grid = PercussionBeatGrid::new(
            "beat_times",
            vec![0.0, 0.5, 1.0, 1.5, 2.0],
            vec![],
            120.0,
            0.9,
            0.0,
        );
        // Beat 0 at 0.0s, beat 1 at 0.5s, etc.
        let beat = grid.try_map_seconds_to_beat(0.25).unwrap();
        assert!((beat - 0.5).abs() < 0.01); // Halfway between beat 0 and 1
    }

    #[test]
    fn test_energy_at_beat() {
        let data = PercussionAnalysisData::new(
            "test",
            120.0,
            vec![],
            0.9,
            None,
            Some(vec![0.0, 0.5, 1.0]),
        );
        assert_eq!(data.energy_at_beat(0.0), 0.0);
        assert_eq!(data.energy_at_beat(0.5), 0.25);
        assert_eq!(data.energy_at_beat(1.0), 0.5);
        assert_eq!(data.energy_at_beat(2.0), 1.0);
    }

    #[test]
    fn test_energy_at_beat_no_envelope() {
        let data = PercussionAnalysisData::new("test", 120.0, vec![], 0.9, None, None);
        assert_eq!(data.energy_at_beat(1.0), 1.0);
    }

    #[test]
    fn test_placement_plan() {
        let mut plan = PercussionPlacementPlan::new();
        plan.add_placement(PercussionClipPlacement::new(
            PercussionTriggerType::Kick,
            0,
            None,
            GeneratorType::None,
            2.0,
            0.5,
            0.9,
            1.0,
        ));
        plan.add_placement(PercussionClipPlacement::new(
            PercussionTriggerType::Snare,
            1,
            None,
            GeneratorType::None,
            1.0,
            0.75,
            0.8,
            0.5,
        ));
        plan.sort_placements();
        assert_eq!(plan.placements()[0].start_beat, 1.0);
        assert_eq!(plan.placements()[1].start_beat, 2.0);
    }

    #[test]
    fn test_clip_binding_with_video_clip_id() {
        let binding = PercussionClipBinding::new(
            PercussionTriggerType::Kick,
            0,
            None,
            GeneratorType::None,
            0.5,
            0.0,
        );
        let resolved = binding.with_video_clip_id("clip_123");
        assert_eq!(resolved.video_clip_id.as_deref(), Some("clip_123"));
        assert_eq!(resolved.trigger_type, PercussionTriggerType::Kick);
    }
}
