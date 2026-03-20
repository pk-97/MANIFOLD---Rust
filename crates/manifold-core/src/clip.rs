use serde::{Deserialize, Serialize};
use crate::types::GeneratorType;
use crate::effects::{EffectInstance, EffectGroup, ParamEnvelope};

/// A single clip on the timeline. Beat-primary timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineClip {
    pub id: String,
    #[serde(default)]
    pub video_clip_id: String,
    #[serde(default)]
    pub layer_index: i32,

    // ── Beat-primary timing (source of truth) ──
    #[serde(default)]
    pub start_beat: f32,
    #[serde(default)]
    pub duration_beats: f32,

    // ── Seconds (video source offset, BPM-independent) ──
    #[serde(default)]
    pub in_point: f32,

    // ── Metadata ──
    #[serde(default)]
    pub recorded_bpm: f32,
    #[serde(default)]
    pub is_locked: bool,
    #[serde(default)]
    pub is_muted: bool,
    #[serde(default)]
    pub invert_colors: bool,

    // ── Generator type ──
    #[serde(default)]
    pub generator_type: GeneratorType,

    // ── Transform ──
    #[serde(default)]
    pub translate_x: f32,
    #[serde(default)]
    pub translate_y: f32,
    #[serde(default = "default_one")]
    pub scale: f32,
    #[serde(default)]
    pub rotation: f32,

    // ── Looping ──
    #[serde(default)]
    pub is_looping: bool,
    #[serde(default)]
    pub loop_duration_beats: f32,

    // ── MIDI tick ──
    #[serde(default)]
    pub start_absolute_tick: i32,
    #[serde(default)]
    pub has_start_absolute_tick: bool,

    // ── Effects & modulation ──
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_groups: Option<Vec<EffectGroup>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelopes: Option<Vec<ParamEnvelope>>,

    // ── Legacy flat generator params (V1.0.0 clips) ──
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genRotSpeedXY")]
    pub legacy_gen_rot_speed_xy: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genRotSpeedZW")]
    pub legacy_gen_rot_speed_zw: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genRotSpeedXW")]
    pub legacy_gen_rot_speed_xw: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genLineThickness")]
    pub legacy_gen_line_thickness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "genProjDistance")]
    pub legacy_gen_proj_distance: Option<f32>,
}

impl TimelineClip {
    pub fn end_beat(&self) -> f32 {
        self.start_beat + self.duration_beats
    }

    pub fn is_generator(&self) -> bool {
        self.generator_type != GeneratorType::None
    }

    pub fn is_active_at_beat(&self, beat: f32) -> bool {
        beat >= self.start_beat && beat < self.end_beat()
    }

    pub fn overlaps_with(&self, other: &TimelineClip) -> bool {
        if other.layer_index != self.layer_index {
            return false;
        }
        self.start_beat < other.end_beat() && self.end_beat() > other.start_beat
    }

    pub fn has_any_effect(&self) -> bool {
        self.invert_colors
            || self.translate_x != 0.0
            || self.translate_y != 0.0
            || self.scale != 1.0
            || self.rotation != 0.0
            || !self.effects.is_empty()
    }

    pub fn has_modular_effects(&self) -> bool {
        !self.effects.is_empty()
    }

    pub fn has_envelopes(&self) -> bool {
        self.envelopes.as_ref().is_some_and(|e| !e.is_empty())
    }

    /// Deep clone with new ID.
    pub fn clone_with_new_id(&self) -> Self {
        let mut cloned = self.clone();
        cloned.id = crate::short_id();
        cloned
    }

    /// Deep clone with optionally overridden start beat.
    pub fn clone_at(&self, start_beat: Option<f32>) -> Self {
        let mut cloned = self.clone_with_new_id();
        if let Some(beat) = start_beat {
            cloned.start_beat = beat;
        }
        cloned
    }

    // ── Clamped setters (match Unity TimelineClip property setters) ──

    /// Set duration with non-negative clamp. Unity TimelineClip.cs line 82.
    pub fn set_duration_beats(&mut self, v: f32) {
        self.duration_beats = v.max(0.0);
    }

    /// Set in-point with non-negative clamp. Unity TimelineClip.cs line 114.
    pub fn set_in_point(&mut self, v: f32) {
        self.in_point = v.max(0.0);
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
        layer_index: i32,
        start_beat: f32,
        duration_beats: f32,
        in_point: f32,
    ) -> Self {
        Self {
            video_clip_id,
            layer_index,
            start_beat,
            duration_beats: duration_beats.max(0.0),
            in_point: in_point.max(0.0),
            ..Default::default()
        }
    }

    /// Create a new generator clip.
    pub fn new_generator(
        gen_type: GeneratorType,
        layer_index: i32,
        start_beat: f32,
        duration_beats: f32,
    ) -> Self {
        Self {
            generator_type: gen_type,
            layer_index,
            start_beat,
            duration_beats: duration_beats.max(0.0),
            ..Default::default()
        }
    }

    /// Get the effect groups list, creating it if None.
    pub fn effect_groups_mut(&mut self) -> &mut Vec<EffectGroup> {
        if self.effect_groups.is_none() {
            self.effect_groups = Some(Vec::new());
        }
        self.effect_groups.as_mut().unwrap()
    }

    /// Get the envelopes list, creating it if None.
    pub fn envelopes_mut(&mut self) -> &mut Vec<ParamEnvelope> {
        if self.envelopes.is_none() {
            self.envelopes = Some(Vec::new());
        }
        self.envelopes.as_mut().unwrap()
    }

    /// Set scale with clamp. Unity TimelineClip.cs line 179.
    pub fn set_scale(&mut self, v: f32) {
        self.scale = v.max(0.01);
    }

    /// Set loop duration with clamp. Unity TimelineClip.cs line 201.
    pub fn set_loop_duration_beats(&mut self, v: f32) {
        self.loop_duration_beats = v.max(0.0);
    }

    /// Find effect by type. Unity TimelineClip.cs line 230.
    pub fn find_effect(&self, effect_type: crate::types::EffectType) -> Option<&crate::effects::EffectInstance> {
        self.effects.iter().find(|e| e.effect_type == effect_type)
    }

    /// Find effect group by ID. Unity TimelineClip.cs line 249.
    pub fn find_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.effect_groups.as_ref()?.iter().find(|g| g.id == group_id)
    }
}

impl crate::effects::EffectContainer for TimelineClip {
    fn effects(&self) -> &[crate::effects::EffectInstance] {
        &self.effects
    }
    fn effects_mut(&mut self) -> &mut Vec<crate::effects::EffectInstance> {
        &mut self.effects
    }
    fn effect_groups(&self) -> &[crate::effects::EffectGroup] {
        self.effect_groups.as_deref().unwrap_or(&[])
    }
    fn effect_groups_mut(&mut self) -> &mut Vec<crate::effects::EffectGroup> {
        self.effect_groups_mut()
    }
    fn has_modular_effects(&self) -> bool {
        !self.effects.is_empty()
    }
    fn find_effect(&self, effect_type: crate::types::EffectType) -> Option<&crate::effects::EffectInstance> {
        self.effects.iter().find(|e| e.effect_type == effect_type)
    }
    fn find_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.effect_groups.as_ref()?.iter().find(|g| g.id == group_id)
    }
    fn envelopes(&self) -> &[crate::effects::ParamEnvelope] {
        self.envelopes.as_deref().unwrap_or(&[])
    }
    fn envelopes_mut(&mut self) -> &mut Vec<crate::effects::ParamEnvelope> {
        TimelineClip::envelopes_mut(self)
    }
    fn has_envelopes(&self) -> bool {
        self.envelopes.as_ref().is_some_and(|e| !e.is_empty())
    }
}

impl Default for TimelineClip {
    fn default() -> Self {
        Self {
            id: crate::short_id(),
            video_clip_id: String::new(),
            layer_index: 0,
            start_beat: 0.0,
            duration_beats: 1.0,
            in_point: 0.0,
            recorded_bpm: 0.0,
            is_locked: false,
            is_muted: false,
            invert_colors: false,
            generator_type: GeneratorType::None,
            translate_x: 0.0,
            translate_y: 0.0,
            scale: 1.0,
            rotation: 0.0,
            is_looping: false,
            loop_duration_beats: 0.0,
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

fn default_one() -> f32 { 1.0 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_duration_beats_clamps_negative() {
        let mut clip = TimelineClip::default();
        clip.set_duration_beats(-5.0);
        assert_eq!(clip.duration_beats, 0.0);
    }

    #[test]
    fn test_set_duration_beats_preserves_positive() {
        let mut clip = TimelineClip::default();
        clip.set_duration_beats(4.0);
        assert_eq!(clip.duration_beats, 4.0);
    }

    #[test]
    fn test_set_in_point_clamps_negative() {
        let mut clip = TimelineClip::default();
        clip.set_in_point(-2.0);
        assert_eq!(clip.in_point, 0.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_zero_passthrough() {
        let clip = TimelineClip { recorded_bpm: 0.0, ..Default::default() };
        assert_eq!(clip.recorded_bpm_resolved(), 0.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_clamps_low() {
        let clip = TimelineClip { recorded_bpm: 10.0, ..Default::default() };
        assert_eq!(clip.recorded_bpm_resolved(), 20.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_clamps_high() {
        let clip = TimelineClip { recorded_bpm: 500.0, ..Default::default() };
        assert_eq!(clip.recorded_bpm_resolved(), 300.0);
    }

    #[test]
    fn test_recorded_bpm_resolved_normal() {
        let clip = TimelineClip { recorded_bpm: 120.0, ..Default::default() };
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
        let clip = TimelineClip::new_video("v1".into(), 0, 0.0, -3.0, -1.0);
        assert_eq!(clip.duration_beats, 0.0);
        assert_eq!(clip.in_point, 0.0);
    }

    #[test]
    fn test_new_generator_clamps_duration() {
        let clip = TimelineClip::new_generator(GeneratorType::None, 0, 0.0, -2.0);
        assert_eq!(clip.duration_beats, 0.0);
    }
}
