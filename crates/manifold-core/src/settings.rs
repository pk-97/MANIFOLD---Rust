use crate::effects::{EffectGroup, PresetInstance};
use crate::macro_bank::MacroBank;
use crate::types::{
    ClockAuthority, OscSyncMode, QuantizeMode, ResolutionPreset, TonemapCurve,
};
use crate::units::{Beats, Bpm};
use serde::{Deserialize, Serialize};

// Re-export RT quality types from foundation (UI's accessible home)
pub use manifold_foundation::settings::{
    RtQualityColumn, RtQualitySettings, RtQualityTier, RtRayResolution,
};

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
    /// VSync: lock content thread rendering to display refresh cadence.
    /// When enabled, the content thread renders in sync with the display's vsync
    /// signal via CVDisplayLink (macOS). frame_rate snaps to the nearest clean
    /// display divisor. When disabled, timer-based pacing at frame_rate.
    #[serde(default = "default_true")]
    pub vsync_enabled: bool,
    #[serde(default)]
    pub export_hdr: bool,
    /// Split the video export into one file per section marker inside the
    /// export range (docs/SECTION_EXPORT_DESIGN.md D4). A UI setting read into
    /// `ExportConfig.split_at_section_markers` at export start; the derived
    /// sections themselves never live on the project.
    #[serde(default)]
    pub split_at_section_markers: bool,

    #[serde(default)]
    pub video_library_paths: Vec<String>,
    #[serde(default = "default_10")]
    pub video_player_pool_size: i32,
    #[serde(default = "default_8")]
    pub max_layers: i32,
    #[serde(default)]
    pub default_recording_layer: i32,

    #[serde(default = "default_120")]
    pub bpm: Bpm,
    #[serde(default = "default_4")]
    pub time_signature_numerator: i32,
    #[serde(default = "default_4")]
    pub time_signature_denominator: i32,
    #[serde(default)]
    pub quantize_mode: QuantizeMode,
    #[serde(default)]
    pub resolution_preset: ResolutionPreset,
    /// FSR 1.0 render scale: the pipeline renders at (output × render_scale) and
    /// FSR upscales back to full output resolution. 1.0 = native (FSR disabled).
    /// Valid notched values: 1.0 (native), 0.75 (quality), 0.5 (performance).
    #[serde(default = "default_one")]
    pub render_scale: f32,
    /// Tonemapping curve for display output.
    #[serde(default)]
    pub tonemap_curve: TonemapCurve,

    /// Physical multi-display / totem arrangement (empty = legacy single
    /// canvas at `output_width`/`output_height`, today's behavior,
    /// byte-identical). Skipped on serialize when empty so projects that
    /// never configured a stage layout round-trip byte-identically, matching
    /// `audio_setup`'s convention. See `docs/MULTI_DISPLAY_DESIGN.md`.
    #[serde(default, skip_serializing_if = "crate::stage::StageLayout::is_empty")]
    pub stage_layout: crate::stage::StageLayout,

    #[serde(default = "default_one")]
    pub master_opacity: f32,
    #[serde(default)]
    pub master_effects: Vec<PresetInstance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_effect_groups: Option<Vec<EffectGroup>>,

    #[serde(default = "default_neg_one_i32")]
    pub led_exit_index: i32,
    #[serde(default = "default_one")]
    pub led_brightness: f32,
    /// Linear gain on HDR scene values before the chroma-preserving clip in
    /// the LED slicer. The LED path bypasses the screen tonemap (LEDs have far
    /// more headroom than any TV), so this is the only place HDR peaks get
    /// scaled before the 8-bit DMX clamp. 1.0 = scene 1.0 → LED full on (low
    /// headroom, strobes saturate instantly). Higher values preserve more
    /// highlight headroom at the cost of strobe punch.
    #[serde(default = "default_one")]
    pub led_gain: f32,
    /// Whether the LED output pipeline should be initialised when this project
    /// is loaded (and kept toggled as the user flips the master LED button).
    /// Persistent so an LED-driven show comes back ON without manual re-toggle.
    #[serde(default)]
    pub led_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_clock_source_name: Option<String>,
    #[serde(default)]
    pub clock_authority: ClockAuthority,
    #[serde(default = "default_9001")]
    pub osc_send_port: i32,
    #[serde(default)]
    pub osc_sync_mode: OscSyncMode,

    #[serde(default)]
    pub macro_bank: MacroBank,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ableton_set_context: Option<crate::ableton_mapping::AbletonSetContext>,

    #[serde(default)]
    pub rt_quality: RtQualitySettings,

    #[serde(default = "default_neg_one_f")]
    pub inspector_width: f32,
    #[serde(default = "default_neg_one_f")]
    pub timeline_height_percent: f32,
    #[serde(default = "default_neg_one_f")]
    pub effect_browser_width: f32,
    #[serde(default)]
    pub effect_browser_open: bool,

    // ── Viewport state (saved/restored on project load) ──
    #[serde(default)]
    pub viewport_scroll_x_beats: f32,
    #[serde(default)]
    pub viewport_scroll_y_px: f32,
    #[serde(default = "default_ppb")]
    pub viewport_pixels_per_beat: f32,

    // ── Inspector collapse states (saved/restored on project load) ──
    // Macros default collapsed: new projects and projects predating this field
    // open with the panel closed.
    #[serde(default = "default_true")]
    pub macros_collapsed: bool,
    #[serde(default)]
    pub master_chrome_collapsed: bool,
    #[serde(default)]
    pub layer_chrome_collapsed: bool,
    #[serde(default)]
    pub clip_chrome_collapsed: bool,

    // ── Legacy flat effect fields (V1.0.0) ──
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "bloomAmount"
    )]
    pub legacy_bloom_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "feedbackAmount"
    )]
    pub legacy_feedback_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "pixelSortAmount"
    )]
    pub legacy_pixel_sort_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "kaleidoscopeAmount"
    )]
    pub legacy_kaleidoscope_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "kaleidoscopeSegments"
    )]
    pub legacy_kaleidoscope_segments: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "edgeStretchAmount"
    )]
    pub legacy_edge_stretch_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "edgeStretchSourceWidth"
    )]
    pub legacy_edge_stretch_source_width: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "infiniteZoomAmount"
    )]
    pub legacy_infinite_zoom_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "infiniteZoomSharpness"
    )]
    pub legacy_infinite_zoom_sharpness: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "voronoiPrismAmount"
    )]
    pub legacy_voronoi_prism_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "voronoiPrismCellCount"
    )]
    pub legacy_voronoi_prism_cell_count: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "quadMirrorAmount"
    )]
    pub legacy_quad_mirror_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "ditherAmount"
    )]
    pub legacy_dither_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "ditherAlgorithm"
    )]
    pub legacy_dither_algorithm: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "strobeAmount"
    )]
    pub legacy_strobe_amount: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "strobeRate"
    )]
    pub legacy_strobe_rate: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "strobeMode"
    )]
    pub legacy_strobe_mode: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "masterEffectOrder"
    )]
    pub legacy_master_effect_order: Option<serde_json::Value>,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            output_width: 1920,
            output_height: 1080,
            frame_rate: 60.0,
            vsync_enabled: true,
            export_hdr: false,
            split_at_section_markers: false,
            video_library_paths: Vec::new(),
            video_player_pool_size: 10,
            max_layers: 8,
            default_recording_layer: 0,
            bpm: Bpm::DEFAULT,
            time_signature_numerator: 4,
            time_signature_denominator: 4,
            quantize_mode: QuantizeMode::Off,
            resolution_preset: ResolutionPreset::FHD1080p,
            render_scale: 1.0,
            tonemap_curve: TonemapCurve::AcesNarkowicz,
            stage_layout: crate::stage::StageLayout::default(),
            master_opacity: 1.0,
            master_effects: Vec::new(),
            master_effect_groups: None,
            led_exit_index: -1,
            led_brightness: 1.0,
            led_gain: 1.0,
            led_enabled: false,
            midi_clock_source_name: None,
            clock_authority: ClockAuthority::Internal,
            osc_send_port: 9001,
            osc_sync_mode: OscSyncMode::M4L,
            macro_bank: MacroBank::default(),
            ableton_set_context: None,
            inspector_width: -1.0,
            timeline_height_percent: -1.0,
            effect_browser_width: -1.0,
            effect_browser_open: false,
            viewport_scroll_x_beats: 0.0,
            viewport_scroll_y_px: 0.0,
            viewport_pixels_per_beat: 120.0,
            macros_collapsed: true,
            master_chrome_collapsed: false,
            layer_chrome_collapsed: false,
            clip_chrome_collapsed: false,
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
            rt_quality: RtQualitySettings::default(),
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
    pub fn quantize_beat(&self, beat: Beats) -> Beats {
        let interval = self.get_quantize_interval_beats() as f64;
        if interval <= 0.0 {
            return beat;
        }
        Beats((beat.0 / interval).round() * interval)
    }

    // ── Clamped setters matching Unity ProjectSettings.cs ──

    pub fn set_bpm(&mut self, v: f32) {
        self.bpm = Bpm::clamped(v);
    }
    pub fn set_output_width(&mut self, v: i32) {
        self.output_width = v.max(1);
    }
    pub fn set_output_height(&mut self, v: i32) {
        self.output_height = v.max(1);
    }
    pub fn set_frame_rate(&mut self, v: f32) {
        self.frame_rate = v.max(1.0);
    }
    pub fn set_time_sig_numerator(&mut self, v: i32) {
        self.time_signature_numerator = v.clamp(1, 16);
    }
    pub fn set_time_sig_denominator(&mut self, v: i32) {
        self.time_signature_denominator = v.clamp(1, 16);
    }
    pub fn set_master_opacity(&mut self, v: f32) {
        self.master_opacity = v.clamp(0.0, 1.0);
    }
    pub fn set_video_player_pool_size(&mut self, v: i32) {
        self.video_player_pool_size = v.max(1);
    }
    pub fn set_max_layers(&mut self, v: i32) {
        self.max_layers = v.max(1);
    }
    pub fn set_default_recording_layer(&mut self, v: i32) {
        self.default_recording_layer = v.max(0);
    }
    pub fn set_osc_send_port(&mut self, v: i32) {
        self.osc_send_port = v.clamp(1024, 65535);
    }

    // ── Computed properties ──

    #[must_use]
    pub fn seconds_per_beat(&self) -> f32 {
        60.0 / self.bpm.0
    }
    pub fn seconds_per_bar(&self) -> f32 {
        self.seconds_per_beat() * self.time_signature_numerator as f32
    }
    pub fn get_frame_duration(&self) -> f32 {
        1.0 / self.frame_rate
    }
    pub fn time_to_frame(&self, seconds: f32) -> i32 {
        (seconds * self.frame_rate).floor() as i32
    }
    pub fn frame_to_time(&self, frame: i32) -> f32 {
        frame as f32 / self.frame_rate
    }

    /// Check if any master effect is active. Unity ProjectSettings.cs lines 200-213.
    pub fn has_any_master_effect(&self) -> bool {
        if self.master_opacity < 1.0 {
            return true;
        }
        self.master_effects.iter().any(|e| e.enabled)
    }

    /// Find master effect by type. Unity ProjectSettings.cs lines 230-239.
    pub fn find_master_effect(
        &self,
        effect_type: &crate::preset_type_id::PresetTypeId,
    ) -> Option<&crate::effects::PresetInstance> {
        self.master_effects
            .iter()
            .find(|e| e.effect_type() == effect_type)
    }

    /// Find master effect group by ID. Unity ProjectSettings.cs lines 252-258.
    pub fn find_master_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.master_effect_groups
            .as_ref()?
            .iter()
            .find(|g| g.id == group_id)
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
    fn effects(&self) -> &[crate::effects::PresetInstance] {
        &self.master_effects
    }
    fn effects_mut(&mut self) -> &mut Vec<crate::effects::PresetInstance> {
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
    fn find_effect(
        &self,
        effect_type: &crate::preset_type_id::PresetTypeId,
    ) -> Option<&crate::effects::PresetInstance> {
        self.master_effects
            .iter()
            .find(|e| e.effect_type() == effect_type)
    }
    fn find_effect_group(&self, group_id: &str) -> Option<&crate::effects::EffectGroup> {
        self.master_effect_groups
            .as_ref()?
            .iter()
            .find(|g| g.id == group_id)
    }
}

fn default_1920() -> i32 {
    1920
}
fn default_1080() -> i32 {
    1080
}
fn default_60() -> f32 {
    60.0
}
fn default_10() -> i32 {
    10
}
fn default_8() -> i32 {
    8
}
fn default_120() -> Bpm {
    Bpm::DEFAULT
}
fn default_4() -> i32 {
    4
}
fn default_one() -> f32 {
    1.0
}
fn default_ppb() -> f32 {
    120.0
}
fn default_9001() -> i32 {
    9001
}
fn default_neg_one_f() -> f32 {
    -1.0
}
fn default_neg_one_i32() -> i32 {
    -1
}
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn rt_quality_tier_spp_ladder() {
        assert_eq!(RtQualityTier::UltraLow.spp(), 1);
        assert_eq!(RtQualityTier::Low.spp(), 2);
        assert_eq!(RtQualityTier::Medium.spp(), 4);
        assert_eq!(RtQualityTier::High.spp(), 8);
        assert_eq!(RtQualityTier::ExtraHigh.spp(), 16);
        assert_eq!(RtQualityTier::Ultra.spp(), 32);
    }

    #[test]
    fn rt_quality_tier_spp_always_positive() {
        for tier in [
            RtQualityTier::UltraLow,
            RtQualityTier::Low,
            RtQualityTier::Medium,
            RtQualityTier::High,
            RtQualityTier::ExtraHigh,
            RtQualityTier::Ultra,
        ] {
            assert!(tier.spp() >= 1, "spp() must be >= 1 for {:?}", tier);
        }
    }

    #[test]
    fn rt_quality_live_defaults_match_constants() {
        let live = RtQualityColumn::default();
        // Live defaults from design D2:
        // shadows UltraLow (1), ao/gi Medium (4), reflections High (8), ray Half
        assert_eq!(live.shadows, RtQualityTier::UltraLow);
        assert_eq!(live.ao, RtQualityTier::Medium);
        assert_eq!(live.gi, RtQualityTier::Medium);
        assert_eq!(live.reflections, RtQualityTier::High);
        assert_eq!(live.ray_resolution, RtRayResolution::Half);
        // Verify spp values match today's constants exactly
        assert_eq!(live.shadows.spp(), 1);
        assert_eq!(live.ao.spp(), 4);
        assert_eq!(live.gi.spp(), 4);
        assert_eq!(live.reflections.spp(), 8);
        assert_eq!(live.ray_resolution.fraction(), (1, 2));
    }

    #[test]
    fn rt_quality_export_defaults() {
        let export = RtQualityColumn::export_default();
        // Export defaults from design D2:
        // shadows High (8), ao/gi High (8), reflections ExtraHigh (16), ray Native
        assert_eq!(export.shadows, RtQualityTier::High);
        assert_eq!(export.ao, RtQualityTier::High);
        assert_eq!(export.gi, RtQualityTier::High);
        assert_eq!(export.reflections, RtQualityTier::ExtraHigh);
        assert_eq!(export.ray_resolution, RtRayResolution::Native);
        // Verify spp values
        assert_eq!(export.shadows.spp(), 8);
        assert_eq!(export.ao.spp(), 8);
        assert_eq!(export.gi.spp(), 8);
        assert_eq!(export.reflections.spp(), 16);
        assert_eq!(export.ray_resolution.fraction(), (1, 1));
    }

    #[test]
    fn rt_quality_serde_round_trip() {
        let settings = RtQualitySettings {
            realtime: RtQualityColumn {
                shadows: RtQualityTier::Low,
                ao: RtQualityTier::ExtraHigh,
                gi: RtQualityTier::High,
                reflections: RtQualityTier::Ultra,
                ray_resolution: RtRayResolution::Quarter,
            },
            export: RtQualityColumn {
                shadows: RtQualityTier::ExtraHigh,
                ao: RtQualityTier::Ultra,
                gi: RtQualityTier::ExtraHigh,
                reflections: RtQualityTier::Ultra,
                ray_resolution: RtRayResolution::ThreeQuarter,
            },
        };

        let json = serde_json::to_string(&settings).unwrap();
        let deserialized: RtQualitySettings = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.realtime.shadows, RtQualityTier::Low);
        assert_eq!(deserialized.realtime.ao, RtQualityTier::ExtraHigh);
        assert_eq!(deserialized.realtime.gi, RtQualityTier::High);
        assert_eq!(deserialized.realtime.reflections, RtQualityTier::Ultra);
        assert_eq!(deserialized.realtime.ray_resolution, RtRayResolution::Quarter);

        assert_eq!(deserialized.export.shadows, RtQualityTier::ExtraHigh);
        assert_eq!(deserialized.export.ao, RtQualityTier::Ultra);
        assert_eq!(deserialized.export.gi, RtQualityTier::ExtraHigh);
        assert_eq!(deserialized.export.reflections, RtQualityTier::Ultra);
        assert_eq!(deserialized.export.ray_resolution, RtRayResolution::ThreeQuarter);
    }

    #[test]
    fn rt_quality_missing_field_gets_defaults() {
        // Old JSON without rt_quality field should deserialize to defaults
        let old_json = r#"{
            "outputWidth": 1920,
            "outputHeight": 1080,
            "frameRate": 60.0,
            "vsyncEnabled": true,
            "exportHdr": false,
            "videoLibraryPaths": [],
            "videoPlayerPoolSize": 10,
            "maxLayers": 8,
            "bpm": 120.0,
            "timeSignatureNumerator": 4,
            "timeSignatureDenominator": 4,
            "renderScale": 1.0
        }"#;

        let settings: ProjectSettings = serde_json::from_str(old_json).unwrap();
        let defaults = RtQualitySettings::default();

        assert_eq!(settings.rt_quality.realtime.shadows, defaults.realtime.shadows);
        assert_eq!(settings.rt_quality.realtime.ao, defaults.realtime.ao);
        assert_eq!(settings.rt_quality.realtime.gi, defaults.realtime.gi);
        assert_eq!(settings.rt_quality.realtime.reflections, defaults.realtime.reflections);
        assert_eq!(settings.rt_quality.realtime.ray_resolution, defaults.realtime.ray_resolution);

        assert_eq!(settings.rt_quality.export.shadows, defaults.export.shadows);
        assert_eq!(settings.rt_quality.export.ao, defaults.export.ao);
        assert_eq!(settings.rt_quality.export.gi, defaults.export.gi);
        assert_eq!(settings.rt_quality.export.reflections, defaults.export.reflections);
        assert_eq!(settings.rt_quality.export.ray_resolution, defaults.export.ray_resolution);
    }

    #[test]
    fn rt_ray_resolution_fraction() {
        assert_eq!(RtRayResolution::Quarter.fraction(), (1, 4));
        assert_eq!(RtRayResolution::Half.fraction(), (1, 2));
        assert_eq!(RtRayResolution::ThreeQuarter.fraction(), (3, 4));
        assert_eq!(RtRayResolution::Native.fraction(), (1, 1));
    }
}

