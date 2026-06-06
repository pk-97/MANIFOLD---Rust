use crate::effects::{EffectGroup, PresetInstance, ParamEnvelope};
use crate::id::ClipId;
use crate::units::{Beats, Seconds};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A single clip on the timeline. Beat-primary timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineClip {
    pub id: ClipId,
    #[serde(default)]
    pub video_clip_id: String,

    // ── Beat-primary timing (source of truth) ──
    #[serde(default)]
    pub start_beat: Beats,
    #[serde(default = "default_one_beat")]
    pub duration_beats: Beats,

    // ── Seconds (video source offset, BPM-independent) ──
    #[serde(default)]
    pub in_point: Seconds,

    // ── Metadata ──
    #[serde(default)]
    pub recorded_bpm: f32,
    #[serde(default)]
    pub is_locked: bool,
    #[serde(default)]
    pub is_muted: bool,

    // ── Legacy fields (deserialized from old projects, never written back) ──
    #[serde(default, skip_serializing)]
    pub layer_id: crate::id::LayerId,
    #[serde(default, skip_serializing)]
    pub generator_type: crate::preset_type_id::PresetTypeId,
    #[serde(default, skip_serializing)]
    pub invert_colors: bool,

    // ── Transform ──
    #[serde(default)]
    pub translate_x: f32,
    #[serde(default)]
    pub translate_y: f32,
    #[serde(default = "default_one")]
    pub scale: f32,
    #[serde(default)]
    pub rotation: f32,

    // ── String params (per-clip, for generators that need text/string data) ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string_params: Option<BTreeMap<String, String>>,

    // ── Looping ──
    #[serde(default)]
    pub is_looping: bool,
    #[serde(default)]
    pub loop_duration_beats: Beats,

    // ── MIDI tick ──
    #[serde(default)]
    pub start_absolute_tick: i32,
    #[serde(default)]
    pub has_start_absolute_tick: bool,

    // ── Legacy: per-clip effects removed (Ableton model: effects on layer/master only) ──
    // Fields kept for deserialization of old projects, never written back.
    #[serde(default, skip_serializing)]
    pub effects: Vec<PresetInstance>,
    #[serde(default, skip_serializing)]
    pub effect_groups: Option<Vec<EffectGroup>>,
    #[serde(default, skip_serializing)]
    pub envelopes: Option<Vec<ParamEnvelope>>,

    // ── Legacy flat generator params (V1.0.0 clips) ──
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genRotSpeedXY"
    )]
    pub legacy_gen_rot_speed_xy: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genRotSpeedZW"
    )]
    pub legacy_gen_rot_speed_zw: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genRotSpeedXW"
    )]
    pub legacy_gen_rot_speed_xw: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genLineThickness"
    )]
    pub legacy_gen_line_thickness: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "genProjDistance"
    )]
    pub legacy_gen_proj_distance: Option<f32>,
}

impl TimelineClip {
    #[must_use]
    pub fn end_beat(&self) -> Beats {
        self.start_beat + self.duration_beats
    }

    pub fn is_active_at_beat(&self, beat: Beats) -> bool {
        beat >= self.start_beat && beat < self.end_beat()
    }

    pub fn overlaps_with(&self, other: &TimelineClip) -> bool {
        self.start_beat < other.end_beat() && self.end_beat() > other.start_beat
    }

    pub fn has_any_effect(&self) -> bool {
        self.translate_x != 0.0
            || self.translate_y != 0.0
            || self.scale != 1.0
            || self.rotation != 0.0
    }

    /// Deep clone with new ID.
    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = ClipId::new(crate::short_id());
        cloned
    }

    /// Deep clone with optionally overridden start beat.
    pub fn clone_at(&self, start_beat: Option<Beats>) -> Self {
        let mut cloned = self.clone_with_new_id();
        if let Some(beat) = start_beat {
            cloned.start_beat = beat;
        }
        cloned
    }

    // ── Clamped setters (match Unity TimelineClip property setters) ──

    /// Set duration with non-negative clamp. Unity TimelineClip.cs line 82.
    pub fn set_duration_beats(&mut self, v: Beats) {
        self.duration_beats = v.max(Beats::ZERO);
    }

    /// Set in-point with non-negative clamp. Unity TimelineClip.cs line 114.
    pub fn set_in_point(&mut self, v: Seconds) {
        self.in_point = v.max(Seconds::ZERO);
    }

    /// Resolved recorded BPM: clamped to 20-300 if > 0, else 0.
    /// Unity TimelineClip.cs lines 122-123.
    pub fn recorded_bpm_resolved(&self) -> f32 {
        if self.recorded_bpm > 0.0 {
            self.recorded_bpm.clamp(20.0, 300.0)
        } else {
            0.0
        }
    }

    /// Set recorded BPM with clamping. Unity TimelineClip.cs lines 126-133.
    pub fn set_recorded_bpm(&mut self, v: f32) {
        if v <= 0.0 {
            self.recorded_bpm = 0.0;
        } else {
            self.recorded_bpm = v.clamp(20.0, 300.0);
        }
    }

    /// Resolved start absolute tick: -1 when not available, else max(0, val).
    /// Unity TimelineClip.cs lines 94-95.
    pub fn start_absolute_tick_resolved(&self) -> i32 {
        if self.has_start_absolute_tick {
            self.start_absolute_tick.max(0)
        } else {
            -1
        }
    }

    /// Create a new video clip.
    pub fn new_video(
        video_clip_id: String,
        start_beat: Beats,
        duration_beats: Beats,
        in_point: Seconds,
    ) -> Self {
        Self {
            video_clip_id,
            start_beat,
            duration_beats: duration_beats.max(Beats::ZERO),
            in_point: in_point.max(Seconds::ZERO),
            ..Default::default()
        }
    }

    /// Create a new generator clip.
    pub fn new_generator(start_beat: Beats, duration_beats: Beats) -> Self {
        Self {
            start_beat,
            duration_beats: duration_beats.max(Beats::ZERO),
            ..Default::default()
        }
    }

    /// Set scale with clamp. Unity TimelineClip.cs line 179.
    pub fn set_scale(&mut self, v: f32) {
        self.scale = v.max(0.01);
    }

    /// Set loop duration with clamp. Unity TimelineClip.cs line 201.
    pub fn set_loop_duration_beats(&mut self, v: Beats) {
        self.loop_duration_beats = v.max(Beats::ZERO);
    }
}

impl Default for TimelineClip {
    fn default() -> Self {
        Self {
            id: ClipId::new(crate::short_id()),
            video_clip_id: String::new(),
            layer_id: crate::id::LayerId::default(),
            start_beat: Beats::ZERO,
            duration_beats: Beats::ONE,
            in_point: Seconds::ZERO,
            recorded_bpm: 0.0,
            is_locked: false,
            is_muted: false,
            invert_colors: false,
            generator_type: crate::preset_type_id::PresetTypeId::NONE,
            translate_x: 0.0,
            translate_y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            string_params: None,
            is_looping: false,
            loop_duration_beats: Beats::ZERO,
            start_absolute_tick: 0,
            has_start_absolute_tick: false,
            effects: Vec::new(),
            effect_groups: None,
            envelopes: None,
            legacy_gen_rot_speed_xy: None,
            legacy_gen_rot_speed_zw: None,
            legacy_gen_rot_speed_xw: None,
            legacy_gen_line_thickness: None,
            legacy_gen_proj_distance: None,
        }
    }
}

fn default_one() -> f32 {
    1.0
}
fn default_one_beat() -> Beats {
    Beats::ONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_duration_beats_clamps_negative() {
        let mut clip = TimelineClip::default();
        clip.set_duration_beats(Beats(-5.0));
        assert_eq!(clip.duration_beats, Beats(0.0));
    }

    #[test]
    fn test_set_duration_beats_preserves_positive() {
        let mut clip = TimelineClip::default();
        clip.set_duration_beats(Beats(4.0));
        assert_eq!(clip.duration_beats, Beats(4.0));
    }

    #[test]
    fn test_set_in_point_clamps_negative() {
        let mut clip = TimelineClip::default();
        clip.set_in_point(Seconds(-2.0));
        assert_eq!(clip.in_point, Seconds(0.0));
    }

    #[test]
    fn test_recorded_bpm_resolved_zero_passthrough() {
        let clip = TimelineClip {
            recorded_bpm: 0.0,
            ..Default::default()
        };
        assert_eq!(clip.recorded_bpm_resolved(), 0.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_clamps_low() {
        let clip = TimelineClip {
            recorded_bpm: 10.0,
            ..Default::default()
        };
        assert_eq!(clip.recorded_bpm_resolved(), 20.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_clamps_high() {
        let clip = TimelineClip {
            recorded_bpm: 500.0,
            ..Default::default()
        };
        assert_eq!(clip.recorded_bpm_resolved(), 300.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_normal() {
        let clip = TimelineClip {
            recorded_bpm: 120.0,
            ..Default::default()
        };
        assert_eq!(clip.recorded_bpm_resolved(), 120.0);
    }

    #[test]
    fn test_set_recorded_bpm_clamps() {
        let mut clip = TimelineClip::default();
        clip.set_recorded_bpm(-1.0);
        assert_eq!(clip.recorded_bpm, 0.0);
        clip.set_recorded_bpm(10.0);
        assert_eq!(clip.recorded_bpm, 20.0);
        clip.set_recorded_bpm(500.0);
        assert_eq!(clip.recorded_bpm, 300.0);
    }

    #[test]
    fn test_start_absolute_tick_resolved_not_available() {
        let clip = TimelineClip {
            has_start_absolute_tick: false,
            start_absolute_tick: 42,
            ..Default::default()
        };
        assert_eq!(clip.start_absolute_tick_resolved(), -1);
    }

    #[test]
    fn test_start_absolute_tick_resolved_available() {
        let clip = TimelineClip {
            has_start_absolute_tick: true,
            start_absolute_tick: 48,
            ..Default::default()
        };
        assert_eq!(clip.start_absolute_tick_resolved(), 48);
    }

    #[test]
    fn test_start_absolute_tick_resolved_clamps_negative() {
        let clip = TimelineClip {
            has_start_absolute_tick: true,
            start_absolute_tick: -5,
            ..Default::default()
        };
        assert_eq!(clip.start_absolute_tick_resolved(), 0);
    }

    #[test]
    fn test_new_video_clamps_duration() {
        let clip = TimelineClip::new_video("v1".into(), Beats(0.0), Beats(-3.0), Seconds(-1.0));
        assert_eq!(clip.duration_beats, Beats(0.0));
        assert_eq!(clip.in_point, Seconds(0.0));
    }

    #[test]
    fn test_new_generator_clamps_duration() {
        let clip = TimelineClip::new_generator(Beats(0.0), Beats(-2.0));
        assert_eq!(clip.duration_beats, Beats(0.0));
    }
}
