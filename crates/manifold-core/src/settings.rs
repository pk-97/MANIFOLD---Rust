use serde::{Deserialize, Serialize};
use crate::types::{ClockAuthority, QuantizeMode, ResolutionPreset};
use crate::effects::{EffectInstance, EffectGroup};

/// Project-wide settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettings {
    #[serde(default = "default_1920")]
    pub output_width: i32,
    #[serde(default = "default_1080")]
    pub output_height: i32,
    #[serde(default = "default_60")]
    pub frame_rate: f32,
    #[serde(default)]
    pub export_hdr: bool,

    #[serde(default)]
    pub video_library_paths: Vec<String>,
    #[serde(default = "default_10")]
    pub video_player_pool_size: i32,
    #[serde(default = "default_8")]
    pub max_layers: i32,
    #[serde(default)]
    pub default_recording_layer: i32,

    #[serde(default = "default_120")]
    pub bpm: f32,
    #[serde(default = "default_4")]
    pub time_signature_numerator: i32,
    #[serde(default = "default_4")]
    pub time_signature_denominator: i32,
    #[serde(default)]
    pub quantize_mode: QuantizeMode,
    #[serde(default)]
    pub resolution_preset: ResolutionPreset,

    #[serde(default = "default_one")]
    pub master_opacity: f32,
    #[serde(default)]
    pub master_effects: Vec<EffectInstance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_effect_groups: Option<Vec<EffectGroup>>,

    #[serde(default = "default_neg_one_i32")]
    pub led_exit_index: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_clock_source_name: Option<String>,
    #[serde(default)]
    pub clock_authority: ClockAuthority,
    #[serde(default = "default_9001")]
    pub osc_send_port: i32,

    #[serde(default = "default_neg_one_f")]
    pub inspector_width: f32,
    #[serde(default = "default_neg_one_f")]
    pub timeline_height_percent: f32,
    #[serde(default = "default_neg_one_f")]
    pub effect_browser_width: f32,
    #[serde(default)]
    pub effect_browser_open: bool,

    // ── Legacy flat effect fields (V1.0.0) ──
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "bloomAmount")]
    pub legacy_bloom_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "feedbackAmount")]
    pub legacy_feedback_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "pixelSortAmount")]
    pub legacy_pixel_sort_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "kaleidoscopeAmount")]
    pub legacy_kaleidoscope_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "kaleidoscopeSegments")]
    pub legacy_kaleidoscope_segments: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "edgeStretchAmount")]
    pub legacy_edge_stretch_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "edgeStretchSourceWidth")]
    pub legacy_edge_stretch_source_width: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "infiniteZoomAmount")]
    pub legacy_infinite_zoom_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "infiniteZoomSharpness")]
    pub legacy_infinite_zoom_sharpness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "voronoiPrismAmount")]
    pub legacy_voronoi_prism_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "voronoiPrismCellCount")]
    pub legacy_voronoi_prism_cell_count: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "quadMirrorAmount")]
    pub legacy_quad_mirror_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ditherAmount")]
    pub legacy_dither_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ditherAlgorithm")]
    pub legacy_dither_algorithm: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "strobeAmount")]
    pub legacy_strobe_amount: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "strobeRate")]
    pub legacy_strobe_rate: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "strobeMode")]
    pub legacy_strobe_mode: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "masterEffectOrder")]
    pub legacy_master_effect_order: Option<serde_json::Value>,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            output_width: 1920,
            output_height: 1080,
            frame_rate: 60.0,
            export_hdr: false,
            video_library_paths: Vec::new(),
            video_player_pool_size: 10,
            max_layers: 8,
            default_recording_layer: 0,
            bpm: 120.0,
            time_signature_numerator: 4,
            time_signature_denominator: 4,
            quantize_mode: QuantizeMode::Off,
            resolution_preset: ResolutionPreset::FHD1080p,
            master_opacity: 1.0,
            master_effects: Vec::new(),
            master_effect_groups: None,
            led_exit_index: -1,
            midi_clock_source_name: None,
            clock_authority: ClockAuthority::Internal,
            osc_send_port: 9001,
            inspector_width: -1.0,
            timeline_height_percent: -1.0,
            effect_browser_width: -1.0,
            effect_browser_open: false,
            legacy_bloom_amount: None,
            legacy_feedback_amount: None,
            legacy_pixel_sort_amount: None,
            legacy_kaleidoscope_amount: None,
            legacy_kaleidoscope_segments: None,
            legacy_edge_stretch_amount: None,
            legacy_edge_stretch_source_width: None,
            legacy_infinite_zoom_amount: None,
            legacy_infinite_zoom_sharpness: None,
            legacy_voronoi_prism_amount: None,
            legacy_voronoi_prism_cell_count: None,
            legacy_quad_mirror_amount: None,
            legacy_dither_amount: None,
            legacy_dither_algorithm: None,
            legacy_strobe_amount: None,
            legacy_strobe_rate: None,
            legacy_strobe_mode: None,
            legacy_master_effect_order: None,
        }
    }
}

impl ProjectSettings {
    /// Get the quantize interval in beats based on current quantize mode and time signature.
    pub fn get_quantize_interval_beats(&self) -> f32 {
        match self.quantize_mode {
            QuantizeMode::Off => 0.0,
            QuantizeMode::QuarterBeat => 0.25,
            QuantizeMode::Beat => 1.0,
            QuantizeMode::Bar => self.time_signature_numerator as f32,
        }
    }

    /// Quantize a beat position to the current quantize grid.
    pub fn quantize_beat(&self, beat: f32) -> f32 {
        let interval = self.get_quantize_interval_beats();
        if interval <= 0.0 {
            return beat;
        }
        (beat / interval).round() * interval
    }

    // ── Clamped setters matching Unity ProjectSettings.cs ──

    pub fn set_bpm(&mut self, v: f32) { self.bpm = v.clamp(20.0, 300.0); }
    pub fn set_output_width(&mut self, v: i32) { self.output_width = v.max(1); }
    pub fn set_output_height(&mut self, v: i32) { self.output_height = v.max(1); }
    pub fn set_frame_rate(&mut self, v: f32) { self.frame_rate = v.max(1.0); }
    pub fn set_time_sig_numerator(&mut self, v: i32) { self.time_signature_numerator = v.clamp(1, 16); }
    pub fn set_time_sig_denominator(&mut self, v: i32) { self.time_signature_denominator = v.clamp(1, 16); }
    pub fn set_master_opacity(&mut self, v: f32) { self.master_opacity = v.clamp(0.0, 1.0); }
    pub fn set_video_player_pool_size(&mut self, v: i32) { self.video_player_pool_size = v.max(1); }
    pub fn set_max_layers(&mut self, v: i32) { self.max_layers = v.max(1); }
    pub fn set_default_recording_layer(&mut self, v: i32) { self.default_recording_layer = v.max(0); }
    pub fn set_osc_send_port(&mut self, v: i32) { self.osc_send_port = v.clamp(1024, 65535); }

    // ── Computed properties ──

    pub fn seconds_per_beat(&self) -> f32 { 60.0 / self.bpm }
    pub fn seconds_per_bar(&self) -> f32 { self.seconds_per_beat() * self.time_signature_numerator as f32 }
    pub fn get_frame_duration(&self) -> f32 { 1.0 / self.frame_rate }
    pub fn time_to_frame(&self, seconds: f32) -> i32 { (seconds * self.frame_rate).floor() as i32 }
    pub fn frame_to_time(&self, frame: i32) -> f32 { frame as f32 / self.frame_rate }

    /// Check if any master effect is active. Unity ProjectSettings.cs lines 200-213.
    pub fn has_any_master_effect(&self) -> bool {
        if self.master_opacity < 1.0 { return true; }
        self.master_effects.iter().any(|e| e.enabled)
    }

    /// Find master effect by type. Unity ProjectSettings.cs lines 230-239.
    pub fn find_master_effect(&self, effect_type: crate::types::EffectType) -> Option<&crate::effects::EffectInstance> {
        self.master_effects.iter().find(|e| e.effect_type() == effect_type)
    }

    /// Find master effect group by ID. Unity ProjectSettings.cs lines 252-258.
    pub fn find_master_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.master_effect_groups.as_ref()?.iter().find(|g| g.id == group_id)
    }

    // ── Video library paths ──

    pub fn add_video_library_path(&mut self, path: String) {
        if !self.video_library_paths.contains(&path) {
            self.video_library_paths.push(path);
        }
    }
    pub fn remove_video_library_path(&mut self, path: &str) {
        self.video_library_paths.retain(|p| p != path);
    }
    pub fn clear_video_library_paths(&mut self) {
        self.video_library_paths.clear();
    }

    /// Get effects list mutably, creating if None on master.
    pub fn master_effect_groups_mut(&mut self) -> &mut Vec<EffectGroup> {
        if self.master_effect_groups.is_none() {
            self.master_effect_groups = Some(Vec::new());
        }
        self.master_effect_groups.as_mut().unwrap()
    }
}

impl crate::effects::EffectContainer for ProjectSettings {
    fn effects(&self) -> &[crate::effects::EffectInstance] {
        &self.master_effects
    }
    fn effects_mut(&mut self) -> &mut Vec<crate::effects::EffectInstance> {
        &mut self.master_effects
    }
    fn effect_groups(&self) -> &[crate::effects::EffectGroup] {
        self.master_effect_groups.as_deref().unwrap_or(&[])
    }
    fn effect_groups_mut(&mut self) -> &mut Vec<crate::effects::EffectGroup> {
        self.master_effect_groups_mut()
    }
    fn has_modular_effects(&self) -> bool {
        !self.master_effects.is_empty()
    }
    fn find_effect(&self, effect_type: crate::types::EffectType) -> Option<&crate::effects::EffectInstance> {
        self.master_effects.iter().find(|e| e.effect_type() == effect_type)
    }
    fn find_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.master_effect_groups.as_ref()?.iter().find(|g| g.id == group_id)
    }
    fn envelopes(&self) -> &[crate::effects::ParamEnvelope] {
        &[]
    }
    fn envelopes_mut(&mut self) -> &mut Vec<crate::effects::ParamEnvelope> {
        // ProjectSettings doesn't have envelopes in Unity
        // This is needed for trait completeness
        panic!("ProjectSettings does not have envelopes");
    }
    fn has_envelopes(&self) -> bool {
        false
    }
}

fn default_1920() -> i32 { 1920 }
fn default_1080() -> i32 { 1080 }
fn default_60() -> f32 { 60.0 }
fn default_10() -> i32 { 10 }
fn default_8() -> i32 { 8 }
fn default_120() -> f32 { 120.0 }
fn default_4() -> i32 { 4 }
fn default_one() -> f32 { 1.0 }
fn default_9001() -> i32 { 9001 }
fn default_neg_one_f() -> f32 { -1.0 }
fn default_neg_one_i32() -> i32 { -1 }
