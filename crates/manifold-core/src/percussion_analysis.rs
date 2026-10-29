// The data Detect produces and plans from: hits, the bindings that turn them into clips, and the
// clip-anchored time mapping.

use serde::{Deserialize, Serialize};

use crate::id::LayerId;
use crate::preset_type_id::PresetTypeId;
use crate::units::{Beats, Bpm, Seconds};

// ─── PercussionTriggerType ───

/// The instruments Detect places triggers for: the families the shipped trained detectors find.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PercussionTriggerType {
    Kick,
    Snare,
}

// ─── PercussionEvent ───

/// One detected hit in the source file's seconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionEvent {
    pub trigger_type: PercussionTriggerType,
    pub time_seconds: f32,
    pub confidence: f32,
}

impl PercussionEvent {
    pub fn new(trigger_type: PercussionTriggerType, time_seconds: f32, confidence: f32) -> Self {
        Self {
            trigger_type,
            time_seconds: time_seconds.max(0.0),
            confidence: confidence.clamp(0.0, 1.0),
        }
    }
}

// ─── PercussionAnalysisData ───

/// One Detect run's hits on an audio clip's file, in time order. Cached on the clip so the inspector's knobs
/// re-plan without re-running the detectors.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PercussionAnalysisData {
    pub events: Vec<PercussionEvent>,
}

impl PercussionAnalysisData {
    pub fn new(mut events: Vec<PercussionEvent>) -> Self {
        events.sort_by(|a, b| a.time_seconds.total_cmp(&b.time_seconds));
        Self { events }
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
    pub generator_type: PresetTypeId,
    pub duration_beats: Beats,
    pub minimum_confidence: f32,
    /// Explicit target layer chosen by the audio-clip inspector. When set and it
    /// resolves to an existing layer, placement routes there directly (bypassing
    /// the trigger-name layout). `None` = route by trigger name (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_layer: Option<LayerId>,
}

impl PercussionClipBinding {
    pub fn new(
        trigger_type: PercussionTriggerType,
        layer_index: i32,
        generator_type: PresetTypeId,
        duration_beats: Beats,
        minimum_confidence: f32,
    ) -> Self {
        Self {
            trigger_type,
            layer_index: layer_index.max(0),
            generator_type,
            duration_beats: duration_beats.max(Beats::ZERO),
            minimum_confidence: minimum_confidence.clamp(0.0, 1.0),
            target_layer: None,
        }
    }

    pub fn uses_generator(&self) -> bool {
        self.generator_type != PresetTypeId::NONE
    }
}

// ─── ClipDetectionAnchor ───

/// Maps a source-file time to a timeline beat for per-clip detection, folding in
/// where the clip sits (`start_beat`), how much of the file it skips (`in_point`),
/// and warp.
///
/// Warped clips map linearly at their source BPM. Native-speed clips retain
/// the tempo map so detection uses the same clock as playback:
/// - **warp on** (`recorded_bpm > 0`): `60 / recorded_bpm`. Warp locks the
///   source's musical tempo to the project, so one source-beat is one
///   timeline-beat and a trigger lands on what is heard.
/// - **warp off**: source seconds are integrated through the project tempo map.
///   The flat constructor remains available for legacy serialized options.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipDetectionAnchor {
    pub start_beat: Beats,
    pub end_beat: Beats,
    pub in_point: Seconds,
    pub seconds_per_beat: f32,
    /// Native-speed clips need the whole map, not the BPM at the playhead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo_map: Option<crate::tempo::TempoMap>,
}

impl ClipDetectionAnchor {
    /// Build from a clip's geometry and the project tempo.
    pub fn new(
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
        recorded_bpm: f32,
        project_bpm: Bpm,
    ) -> Self {
        let effective_bpm = if recorded_bpm > 0.0 {
            recorded_bpm
        } else {
            project_bpm.0.max(1.0)
        };
        Self {
            start_beat,
            end_beat: start_beat + duration_beats,
            in_point: in_point.max(Seconds::ZERO),
            seconds_per_beat: 60.0 / effective_bpm,
            tempo_map: None,
        }
    }

    pub fn from_clip(clip: &crate::clip::TimelineClip, project: &crate::project::Project) -> Self {
        let mut anchor = Self::new(
            clip.start_beat,
            clip.duration_beats,
            clip.in_point,
            clip.recorded_bpm_resolved(),
            project.settings.bpm,
        );
        if clip.recorded_bpm_resolved() <= 0.0 {
            anchor.tempo_map = Some(project.tempo_map.clone());
        }
        anchor
    }

    /// Map a source-file time to a timeline beat, or `None` if the event falls
    /// outside the clip's trimmed window (before `in_point` or past the end).
    pub fn map_source_seconds(&self, source_seconds: Seconds) -> Option<Beats> {
        if !source_seconds.0.is_finite()
            || source_seconds < self.in_point
            || self.seconds_per_beat <= 0.0
        {
            return None;
        }
        let delta = source_seconds - self.in_point;
        let beats = if let Some(map) = &self.tempo_map {
            crate::tempo::SourceClock::new(map, Bpm(60.0 / self.seconds_per_beat), None)
                .beats_for_unwarped_source(self.start_beat, delta)
        } else {
            Beats(delta.0 / f64::from(self.seconds_per_beat))
        };
        let beat = self.start_beat + beats;
        if !beat.0.is_finite() || beat >= self.end_beat {
            return None;
        }
        Some(beat.max(Beats::ZERO))
    }
}

// ─── PercussionImportOptions ───

/// How Detect turns one clip's hits into trigger clips.
#[derive(Debug, Clone)]
pub struct PercussionImportOptions {
    pub quantize_to_grid: bool,
    pub quantize_step_beats: Beats,
    pub default_clip_duration_beats: Beats,
    pub onset_compensation_seconds: Seconds,
    pub bindings: Vec<PercussionClipBinding>,
    /// Maps the clip's source seconds to timeline beats, warp-aware, and trims to the clip window.
    pub clip_anchor: ClipDetectionAnchor,
}

// ─── PercussionClipPlacement ───

/// Port of Unity PercussionClipPlacement class.
/// Planned clip placement in beat-domain, resolved from one percussion event.
#[derive(Debug, Clone)]
pub struct PercussionClipPlacement {
    pub trigger_type: PercussionTriggerType,
    pub layer_index: i32,
    pub video_clip_id: Option<String>,
    pub generator_type: PresetTypeId,
    pub start_beat: Beats,
    pub duration_beats: Beats,
    pub confidence: f32,
    pub source_time_seconds: f32,
}

impl PercussionClipPlacement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trigger_type: PercussionTriggerType,
        layer_index: i32,
        video_clip_id: Option<String>,
        generator_type: PresetTypeId,
        start_beat: Beats,
        duration_beats: Beats,
        confidence: f32,
        source_time_seconds: f32,
    ) -> Self {
        Self {
            trigger_type,
            layer_index: layer_index.max(0),
            video_clip_id,
            generator_type,
            start_beat: start_beat.max(Beats::ZERO),
            duration_beats: duration_beats.max(Beats::ZERO),
            confidence: confidence.clamp(0.0, 1.0),
            source_time_seconds: source_time_seconds.max(0.0),
        }
    }

    pub fn is_generator(&self) -> bool {
        self.generator_type != PresetTypeId::NONE
    }
}

// ─── PercussionPlacementPlan ───

/// The planner's output: the trigger clips to place, sorted by start beat.
#[derive(Debug, Clone, Default)]
pub struct PercussionPlacementPlan {
    placements: Vec<PercussionClipPlacement>,
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
            .sort_by(|a, b| a.start_beat.0.total_cmp(&b.start_beat.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percussion_event_clamping() {
        let e = PercussionEvent::new(PercussionTriggerType::Kick, -1.0, 2.0);
        assert_eq!(e.time_seconds, 0.0);
        assert_eq!(e.confidence, 1.0);
    }

    #[test]
    fn test_placement_plan() {
        let mut plan = PercussionPlacementPlan::new();
        plan.add_placement(PercussionClipPlacement::new(
            PercussionTriggerType::Kick,
            0,
            None,
            PresetTypeId::NONE,
            Beats(2.0),
            Beats(0.5),
            0.9,
            1.0,
        ));
        plan.add_placement(PercussionClipPlacement::new(
            PercussionTriggerType::Snare,
            1,
            None,
            PresetTypeId::NONE,
            Beats(1.0),
            Beats(0.75),
            0.8,
            0.5,
        ));
        plan.sort_placements();
        assert_eq!(plan.placements()[0].start_beat, Beats(1.0));
        assert_eq!(plan.placements()[1].start_beat, Beats(2.0));
    }

    #[test]
    fn old_binding_json_with_video_clip_id_still_loads() {
        let json = r#"{"triggerType":"Kick","layerIndex":2,"videoClipId":"clip_123","generatorType":"","durationBeats":0.5,"minimumConfidence":0.3}"#;
        let binding: PercussionClipBinding = serde_json::from_str(json).expect("old file loads");
        assert_eq!(binding.layer_index, 2);
    }

    #[test]
    fn anchor_warp_on_locks_source_beats_to_timeline() {
        // 120-BPM clip on a 128-BPM project: a hit 0.5s into the file is 1
        // source-beat in, which warp places at timeline beat 1 (start + 1).
        let anchor =
            ClipDetectionAnchor::new(Beats::ZERO, Beats(8.0), Seconds::ZERO, 120.0, Bpm(128.0));
        let beat = anchor.map_source_seconds(Seconds(0.5)).unwrap();
        assert!((beat.0 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn anchor_warp_off_uses_project_tempo() {
        // No recorded BPM: native playback, mapped at the project tempo.
        let anchor =
            ClipDetectionAnchor::new(Beats::ZERO, Beats(8.0), Seconds::ZERO, 0.0, Bpm(120.0));
        // 0.5s native at 120 BPM = 1 beat.
        let beat = anchor.map_source_seconds(Seconds(0.5)).unwrap();
        assert!((beat.0 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn anchor_respects_start_beat_and_in_point() {
        // Clip starts at beat 4 and skips the first 2s of the file.
        let anchor =
            ClipDetectionAnchor::new(Beats(4.0), Beats(8.0), Seconds(2.0), 120.0, Bpm(120.0));
        // Before in_point: trimmed off.
        assert!(anchor.map_source_seconds(Seconds(1.5)).is_none());
        // 2.5s file = 0.5s past in_point = 1 beat past start (4) -> beat 5.
        let beat = anchor.map_source_seconds(Seconds(2.5)).unwrap();
        assert!((beat.0 - 5.0).abs() < 1e-6);
    }

    #[test]
    fn anchor_trims_events_past_clip_end() {
        // 2-beat clip (end_beat = 2). A hit mapping past the end is dropped.
        let anchor =
            ClipDetectionAnchor::new(Beats::ZERO, Beats(2.0), Seconds::ZERO, 120.0, Bpm(120.0));
        assert!(anchor.map_source_seconds(Seconds(0.5)).is_some()); // beat 1, inside
        assert!(anchor.map_source_seconds(Seconds(1.0)).is_none()); // beat 2 == end, trimmed
        assert!(anchor.map_source_seconds(Seconds(5.0)).is_none()); // well past
    }
}

#[cfg(test)]
mod audio_anchor_tempo_tests {
    use super::*;
    #[test]
    fn native_detection_uses_the_same_tempo_map_as_playback() {
        let mut project = crate::project::Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(
            Beats::ZERO,
            Bpm(120.0),
            crate::types::TempoPointSource::Manual,
            0.001,
        );
        project.tempo_map.add_or_replace_point(
            Beats(4.0),
            Bpm(60.0),
            crate::types::TempoPointSource::Manual,
            0.001,
        );
        let clip = crate::clip::TimelineClip::new_audio(
            "a.wav".into(),
            Beats(2.0),
            Beats(8.0),
            Seconds(1.0),
            Seconds(10.0),
        );
        let anchor = ClipDetectionAnchor::from_clip(&clip, &project);
        // Start at t=1, source t=1; two source seconds later is timeline t=3 / beat 5.
        assert!((anchor.map_source_seconds(Seconds(3.0)).unwrap().0 - 5.0).abs() < 1e-6);
        let roundtrip: ClipDetectionAnchor =
            serde_json::from_str(&serde_json::to_string(&anchor).unwrap()).unwrap();
        assert_eq!(
            roundtrip.map_source_seconds(Seconds(3.0)),
            anchor.map_source_seconds(Seconds(3.0))
        );
    }
}
