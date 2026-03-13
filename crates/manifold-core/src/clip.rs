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
    #[serde(default = "default_one")]
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
    #[serde(default)]
    pub effect_groups: Option<Vec<EffectGroup>>,
    #[serde(default)]
    pub envelopes: Option<Vec<ParamEnvelope>>,

    // ── Legacy flat generator params (V1.0.0 clips) ──
    #[serde(default, rename = "genRotSpeedXY")]
    pub legacy_gen_rot_speed_xy: Option<f32>,
    #[serde(default, rename = "genRotSpeedZW")]
    pub legacy_gen_rot_speed_zw: Option<f32>,
    #[serde(default, rename = "genRotSpeedXW")]
    pub legacy_gen_rot_speed_xw: Option<f32>,
    #[serde(default, rename = "genLineThickness")]
    pub legacy_gen_line_thickness: Option<f32>,
    #[serde(default, rename = "genProjDistance")]
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
