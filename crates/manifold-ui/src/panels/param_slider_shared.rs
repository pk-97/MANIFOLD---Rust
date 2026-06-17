//! Shared constants, types, and builder functions for parameter slider panels.
//!
//! The unified `ParamCardPanel` (effect + generator kinds) uses identical
//! layout constants, driver/envelope config builders, trim/target handle
//! builders, and formatting helpers across both kinds. This module is the
//! single source of truth for them.

use super::DriverConfigAction;
use super::TrimKind;
use super::param_card::ParamInfo;
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
pub use manifold_core::ableton_mapping::AbletonMappingStatus;

// ── Shared layout constants ─────────────────────────────────────

pub(crate) const ROW_HEIGHT: f32 = 20.0;
pub(crate) const ROW_SPACING: f32 = 4.0;
pub(crate) const PADDING: f32 = 6.0;
pub(crate) const GAP: f32 = 4.0;
pub(crate) const FONT_SIZE: u16 = color::FONT_BODY;

pub(crate) const DE_BUTTON_SIZE: f32 = 20.0;
pub(crate) const DE_BUTTON_GAP: f32 = 2.0;

/// Active tint for the audio-modulation ("A") button + drawer — a clean green,
/// kept distinct from the driver (teal) and envelope (orange) actives. Shares
/// the audio trim-handle green so the whole audio-mod identity reads as one.
pub(crate) const AUDIO_MOD_ACTIVE_C32: crate::node::Color32 = color::AUDIO_TRIM_BAR_C32;

// Total height of the driver drawer container (two button rows + pads). The
// per-row metrics (row height, button gap, horizontal pad) now live in the
// shared `drawer` module, which builds this drawer.
pub(crate) const DRIVER_CONFIG_HEIGHT: f32 = 56.0;
pub(crate) const BEAT_DIV_COUNT: usize = 11;
pub(crate) const WAVEFORM_COUNT: usize = 5;

pub(crate) const ABL_CONFIG_HEIGHT: f32 = 24.0;

/// Height of the per-param audio-modulation drawer — a send-selector row and a
/// feature-selector row (see `build_param_row`). Derived from the shared drawer
/// metrics so the card's reserved height can't drift from what's actually drawn.
pub(crate) fn audio_config_height() -> f32 {
    crate::panels::drawer::uniform_rows_height(2)
}

// Arming the envelope shows two controls: the orange target handle on the
// parameter's own track (the value it's pulled toward) and a single "Decay"
// slider in a one-row drawer (how fast it falls back).
pub(crate) const ENV_CONFIG_HEIGHT: f32 = 30.0;
pub(crate) const ENV_DECAY_LABEL_W: f32 = 50.0;
/// Decay slider full-scale, in beats (0 → this).
pub(crate) const ENV_DECAY_MAX: f32 = 8.0;
/// Default decay for a freshly-armed envelope — mirrors core's
/// `DEFAULT_ENVELOPE_DECAY_BEATS` so the slider shows a usable value at once.
pub(crate) const DEFAULT_ENV_DECAY: f32 = 1.0;

pub(crate) const TRIM_BAR_W: f32 = 4.0;
pub(crate) const TARGET_BAR_W: f32 = 6.0;
pub(crate) const OVERLAY_INSET: f32 = 1.0;

pub(crate) const BEAT_DIV_LABELS: [&str; BEAT_DIV_COUNT] = [
    "1/32", "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32",
];

// Waveform text labels kept for accessibility / tooltips if needed later.
#[allow(dead_code)]
pub(crate) const WAVEFORM_LABELS: [&str; WAVEFORM_COUNT] = ["Sin", "Tri", "Saw", "Sqr", "Rnd"];

// ── Shared node ID structs ──────────────────────────────────────

pub(crate) struct DriverConfigIds {
    pub(crate) _container_id: i32,
    pub(crate) beat_div_btn_ids: [i32; BEAT_DIV_COUNT],
    pub(crate) dot_btn_id: i32,
    pub(crate) triplet_btn_id: i32,
    pub(crate) wave_btn_ids: [i32; WAVEFORM_COUNT],
    pub(crate) reverse_btn_id: i32,
}

/// The orange envelope target handle on a parameter's slider track — sets the
/// depth (`target_normalized`) the envelope pulls the value toward, shown in the
/// parameter's own range.
pub(crate) struct EnvelopeTargetIds {
    pub(crate) target_bar_id: i32,
}

/// The envelope drawer — a single "Decay" slider (`decay_beats`).
pub(crate) struct EnvelopeConfigIds {
    pub(crate) _container_id: i32,
    pub(crate) decay_slider: SliderNodeIds,
}

#[derive(Clone, Copy)]
pub(crate) struct TrimHandleIds {
    pub(crate) fill_id: i32,
    pub(crate) min_bar_id: i32,
    pub(crate) max_bar_id: i32,
}

pub(crate) struct AbletonConfigIds {
    pub(crate) _container_id: i32,
    pub(crate) invert_btn_id: i32,
}

/// Display data for an Ableton-mapped parameter.
/// Constructed in state_sync, consumed by effect_card and gen_param.
#[derive(Debug, Clone, PartialEq)]
pub struct AbletonMappingDisplay {
    pub macro_name: String,
    /// Stored target track name from the mapping address. Surfaced in
    /// the UI so corrupt mappings (where the stored target doesn't match
    /// what the user intended) are visible at a glance — see the
    /// "make corruption visible" thread in feature/unit-types.
    pub track_name: String,
    /// Stored target device name (rack name in Ableton).
    pub device_name: String,
    pub status: AbletonMappingStatus,
    pub inverted: bool,
}

// ── Shared modulation state ─────────────────────────────────────

/// Per-parameter modulation state for the unified `ParamCardPanel` (both kinds).
/// Contains driver expansion, envelope expansion, trim values, the envelope
/// target (`target_norm` — the orange handle) and decay time (`env_decay` — the
/// drawer slider), and driver visual state (beat div, waveform, reversed,
/// dotted, triplet).
pub struct ParamModState {
    pub driver_expanded: Vec<bool>,
    pub envelope_expanded: Vec<bool>,
    pub trim_min: Vec<f32>,
    pub trim_max: Vec<f32>,
    pub target_norm: Vec<f32>,
    /// Envelope decay time in beats.
    pub env_decay: Vec<f32>,
    pub driver_beat_div_idx: Vec<i32>,
    pub driver_waveform_idx: Vec<i32>,
    pub driver_reversed: Vec<bool>,
    pub driver_dotted: Vec<bool>,
    pub driver_triplet: Vec<bool>,

    // ── Audio modulation (per-param + card-level send list) ──
    /// Per-param: an audio modulation exists and is enabled (button highlight +
    /// drawer auto-expands, mirroring the driver).
    pub audio_active: Vec<bool>,
    /// Per-param: index of the selected send in [`Self::audio_send_labels`], or
    /// -1 if the mod's send no longer resolves.
    pub audio_send_idx: Vec<i32>,
    /// Per-param: selected feature index (see [`AUDIO_FEATURE_LABELS`]).
    pub audio_feature_idx: Vec<i32>,
    /// Per-param: audio-mod output sub-range (the green trim handles), 0..1 of the
    /// slider's travel. Mirrors `trim_min`/`trim_max` for drivers — the audio
    /// drives only this slice of the param's range.
    pub audio_range_min: Vec<f32>,
    pub audio_range_max: Vec<f32>,
    /// Per-param: audio-mod invert (`AudioModShape::invert`) — drives the "Inv"
    /// toggle in the drawer (loud → low).
    pub audio_invert: Vec<bool>,
    /// Card-level: available send labels (same for every row on the card).
    pub audio_send_labels: Vec<String>,
    /// Card-level: send ids parallel to `audio_send_labels` — turns a selected
    /// drawer index into the id an `AudioModSetSource` command needs.
    pub audio_send_ids: Vec<manifold_core::AudioSendId>,
}

/// Map a feature button index (see [`AUDIO_FEATURE_LABELS`]) to its
/// `AudioFeature`. Out-of-range falls back to low-band energy.
pub(crate) fn audio_feature_from_index(idx: usize) -> manifold_core::AudioFeature {
    use manifold_core::audio_mod::{AudioBand, AudioFeature};
    match idx {
        0 => AudioFeature::Amplitude,
        1 => AudioFeature::BandEnergy(AudioBand::Low),
        2 => AudioFeature::BandEnergy(AudioBand::Mid),
        3 => AudioFeature::BandEnergy(AudioBand::High),
        4 => AudioFeature::Centroid,
        5 => AudioFeature::Flatness,
        6 => AudioFeature::Flux,
        7 => AudioFeature::Onset,
        _ => AudioFeature::Amplitude,
    }
}

/// The feature options exposed in the per-slider audio drawer, in button order.
/// Index maps to an `AudioFeature` in the card's click handler. "Amp" is the
/// overall level (the default); Lo/Mid/Hi are the energy bands; Bri is spectral
/// centroid (brightness); Nsy is flatness (tonal→noisy); Flx is spectral flux
/// (continuous change); On is onset (the discrete hit).
pub(crate) const AUDIO_FEATURE_LABELS: [&str; 8] =
    ["Amp", "Lo", "Mid", "Hi", "Bri", "Nsy", "Flx", "On"];

/// Audio-modulation display state for one card, assembled in `state_sync` and
/// applied to [`ParamModState`] via [`ParamModState::sync_audio`]. Bundled so
/// `ParamCardConfig` gains one field, not five.
#[derive(Debug, Default, Clone)]
pub struct AudioCardState {
    /// Per-param: mod exists and is enabled.
    pub active: Vec<bool>,
    /// Per-param: the mod's send id, if any. Resolved to an index into
    /// `send_ids` by [`ParamModState::sync_audio`].
    pub send_id: Vec<Option<manifold_core::AudioSendId>>,
    /// Per-param: selected feature index (0..3).
    pub feature_idx: Vec<i32>,
    /// Per-param: the mod's output sub-range (`AudioModShape::range_min/max`).
    pub range_min: Vec<f32>,
    pub range_max: Vec<f32>,
    /// Per-param: the mod's invert flag (`AudioModShape::invert`).
    pub invert: Vec<bool>,
    /// Card-level: available send labels.
    pub send_labels: Vec<String>,
    /// Card-level: send ids parallel to `send_labels` — what the click handler
    /// turns a selected index into for the `AudioModSetSource` command.
    pub send_ids: Vec<manifold_core::AudioSendId>,
}

impl ParamModState {
    pub fn allocate(param_count: usize) -> Self {
        Self {
            driver_expanded: vec![false; param_count],
            envelope_expanded: vec![false; param_count],
            trim_min: vec![0.0; param_count],
            trim_max: vec![1.0; param_count],
            target_norm: vec![0.5; param_count],
            env_decay: vec![DEFAULT_ENV_DECAY; param_count],
            driver_beat_div_idx: vec![-1; param_count],
            driver_waveform_idx: vec![-1; param_count],
            driver_reversed: vec![false; param_count],
            driver_dotted: vec![false; param_count],
            driver_triplet: vec![false; param_count],
            audio_active: vec![false; param_count],
            audio_send_idx: vec![-1; param_count],
            audio_feature_idx: vec![0; param_count],
            audio_range_min: vec![0.0; param_count],
            audio_range_max: vec![1.0; param_count],
            audio_invert: vec![false; param_count],
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
        }
    }

    /// Sync audio-modulation display state from the card config.
    pub fn sync_audio(&mut self, n: usize, audio: &AudioCardState) {
        for i in 0..n {
            self.audio_active[i] = audio.active.get(i).copied().unwrap_or(false);
            self.audio_feature_idx[i] = audio.feature_idx.get(i).copied().unwrap_or(0);
            self.audio_range_min[i] = audio.range_min.get(i).copied().unwrap_or(0.0);
            self.audio_range_max[i] = audio.range_max.get(i).copied().unwrap_or(1.0);
            self.audio_invert[i] = audio.invert.get(i).copied().unwrap_or(false);
            self.audio_send_idx[i] = audio
                .send_id
                .get(i)
                .and_then(|o| o.as_ref())
                .and_then(|sid| audio.send_ids.iter().position(|s| s == sid))
                .map(|p| p as i32)
                .unwrap_or(-1);
        }
        self.audio_send_labels = audio.send_labels.clone();
        self.audio_send_ids = audio.send_ids.clone();
    }

    /// Sync driver/envelope/trim/target/decay state from config vectors.
    /// `n` is the param count. Reads from config slices with fallback defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn sync_from_config(
        &mut self,
        n: usize,
        driver_active: &[bool],
        envelope_active: &[bool],
        trim_min: &[f32],
        trim_max: &[f32],
        target_norm: &[f32],
        env_decay: &[f32],
        driver_beat_div_idx: &[i32],
        driver_waveform_idx: &[i32],
        driver_reversed: &[bool],
        driver_dotted: &[bool],
        driver_triplet: &[bool],
    ) {
        for i in 0..n {
            self.driver_expanded[i] = driver_active.get(i).copied().unwrap_or(false);
            self.envelope_expanded[i] = envelope_active.get(i).copied().unwrap_or(false);
            self.trim_min[i] = trim_min.get(i).copied().unwrap_or(0.0);
            self.trim_max[i] = trim_max.get(i).copied().unwrap_or(1.0);
            self.target_norm[i] = target_norm.get(i).copied().unwrap_or(1.0);
            self.env_decay[i] = env_decay.get(i).copied().unwrap_or(DEFAULT_ENV_DECAY);
            self.driver_beat_div_idx[i] = driver_beat_div_idx.get(i).copied().unwrap_or(-1);
            self.driver_waveform_idx[i] = driver_waveform_idx.get(i).copied().unwrap_or(-1);
            self.driver_reversed[i] = driver_reversed.get(i).copied().unwrap_or(false);
            self.driver_dotted[i] = driver_dotted.get(i).copied().unwrap_or(false);
            self.driver_triplet[i] = driver_triplet.get(i).copied().unwrap_or(false);
        }
    }
}

// ── Shared drag state ───────────────────────────────────────────

/// Drag tracking state for the unified `ParamCardPanel` (both kinds).
pub(crate) struct ParamDragState {
    pub(crate) dragging_param: i32,
    /// The active modulator trim-range drag, if any: `(kind, param_index,
    /// is_min)`. The three formerly parallel driver/Ableton/audio drag slots
    /// are one slot now — only one trim handle is ever dragged at a time, and
    /// [`TrimKind`] records which modulator's range it is.
    pub(crate) dragging_trim: Option<(TrimKind, usize, bool)>,
    /// The envelope target (orange handle / `target_normalized`) on the track.
    pub(crate) dragging_target_param: i32,
    /// The envelope decay slider (`decay_beats`) in the drawer.
    pub(crate) dragging_decay_param: i32,
}

impl ParamDragState {
    pub(crate) fn new() -> Self {
        Self {
            dragging_param: -1,
            dragging_trim: None,
            dragging_target_param: -1,
            dragging_decay_param: -1,
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging_param >= 0
            || self.dragging_trim.is_some()
            || self.dragging_target_param >= 0
            || self.dragging_decay_param >= 0
    }
}

/// Reposition a trim overlay's three nodes (fill + min/max bars) along a slider
/// track for a new `[min, max]`. The pixel math is identical for driver,
/// Ableton, and audio trims — this is the single copy they all share, so a
/// layout tweak lands once instead of drifting across three near-identical
/// blocks.
pub(crate) fn reposition_trim_bars(
    tree: &mut UITree,
    track_rect: Rect,
    ids: &TrimHandleIds,
    new_min: f32,
    new_max: f32,
) {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let base_x = track_rect.x + OVERLAY_INSET;
    let fill_x = base_x + new_min * usable;
    let fill_w = (new_max - new_min) * usable;
    let fill_h = track_rect.height - OVERLAY_INSET * 2.0;
    tree.set_bounds(
        ids.fill_id as u32,
        Rect::new(fill_x, track_rect.y + OVERLAY_INSET, fill_w, fill_h),
    );
    tree.set_bounds(
        ids.min_bar_id as u32,
        Rect::new(
            base_x + new_min * usable - TRIM_BAR_W * 0.5,
            track_rect.y,
            TRIM_BAR_W,
            track_rect.height,
        ),
    );
    tree.set_bounds(
        ids.max_bar_id as u32,
        Rect::new(
            base_x + new_max * usable - TRIM_BAR_W * 0.5,
            track_rect.y,
            TRIM_BAR_W,
            track_rect.height,
        ),
    );
}

// ── Shared helper functions ─────────────────────────────────────

pub(crate) fn format_param_value(
    val: f32,
    min: f32,
    whole_numbers: bool,
    is_angle: bool,
    value_labels: Option<&[String]>,
) -> String {
    if let Some(labels) = value_labels {
        let idx = ((val - min).round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
        return labels[idx].clone();
    }
    if is_angle {
        // `val` is radians; the user always sees and edits degrees.
        format!("{:.0}°", val.to_degrees())
    } else if whole_numbers {
        format!("{}", val.round() as i32)
    } else {
        format!("{:.2}", val)
    }
}

pub(crate) fn de_btn_style(active: bool, active_color: Color32) -> UIStyle {
    if active {
        UIStyle {
            bg_color: active_color,
            hover_bg_color: Color32::new(
                active_color.r.saturating_add(20),
                active_color.g.saturating_add(20),
                active_color.b.saturating_add(20),
                active_color.a,
            ),
            pressed_bg_color: Color32::new(
                active_color.r.saturating_sub(10),
                active_color.g.saturating_sub(10),
                active_color.b.saturating_sub(10),
                active_color.a,
            ),
            text_color: color::TEXT_WHITE_C32,
            font_size: color::FONT_CAPTION,
            corner_radius: 2.0,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: color::DRIVER_INACTIVE_C32,
            hover_bg_color: color::DRIVER_INACTIVE_HOVER_C32,
            pressed_bg_color: color::DRIVER_INACTIVE_PRESS_C32,
            text_color: color::TEXT_DIMMED_C32,
            font_size: color::FONT_CAPTION,
            corner_radius: 2.0,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

/// Style for driver config buttons (beat div, waveform, dot, triplet, reverse).
/// `font_size` parameter allows callers to specify the font size (effect_card uses 8, gen_param uses FONT_SIZE=10).
pub(crate) fn config_btn_style(active: bool, font_size: u16) -> UIStyle {
    if active {
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::DRIVER_ACTIVE_HOVER_C32,
            pressed_bg_color: color::DRIVER_ACTIVE_PRESS_C32,
            text_color: color::TEXT_WHITE_C32,
            font_size,
            corner_radius: 1.0,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: color::CONFIG_BTN_INACTIVE_C32,
            hover_bg_color: color::CONFIG_BTN_HOVER_C32,
            pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
            text_color: color::TEXT_DIMMED_C32,
            font_size,
            corner_radius: 1.0,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

/// Like `config_btn_style` but uses a custom active color (e.g. Ableton purple).
pub(crate) fn config_btn_style_colored(
    active: bool,
    active_color: Color32,
    font_size: u16,
) -> UIStyle {
    if active {
        UIStyle {
            bg_color: active_color,
            hover_bg_color: Color32::new(
                active_color.r.saturating_add(20),
                active_color.g.saturating_add(20),
                active_color.b.saturating_add(20),
                active_color.a,
            ),
            pressed_bg_color: Color32::new(
                active_color.r.saturating_sub(10),
                active_color.g.saturating_sub(10),
                active_color.b.saturating_sub(10),
                active_color.a,
            ),
            text_color: color::TEXT_WHITE_C32,
            font_size,
            corner_radius: 1.0,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: color::CONFIG_BTN_INACTIVE_C32,
            hover_bg_color: color::CONFIG_BTN_HOVER_C32,
            pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
            text_color: color::TEXT_DIMMED_C32,
            font_size,
            corner_radius: 1.0,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

pub(crate) fn toggle_btn_style(enabled: bool) -> UIStyle {
    if enabled {
        UIStyle {
            bg_color: color::ACCENT_BLUE_C32,
            hover_bg_color: color::ACCENT_BLUE_HOVER_C32,
            pressed_bg_color: color::ACCENT_BLUE_PRESS_C32,
            text_color: color::TEXT_WHITE_C32,
            font_size: color::FONT_CAPTION,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            bg_color: color::BUTTON_INACTIVE_C32,
            hover_bg_color: color::BUTTON_INACTIVE_HOVER_C32,
            pressed_bg_color: color::BUTTON_INACTIVE_PRESS_C32,
            text_color: color::TEXT_DIMMED_C32,
            font_size: color::FONT_CAPTION,
            corner_radius: color::BUTTON_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}

// ── Shared builder functions ────────────────────────────────────

pub(crate) fn build_driver_config(
    tree: &mut UITree,
    parent: i32,
    x: f32,
    y: f32,
    w: f32,
    mod_state: &ParamModState,
    param_idx: usize,
    btn_font_size: u16,
) -> DriverConfigIds {
    use crate::panels::drawer::{self, ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};

    let active_div = mod_state
        .driver_beat_div_idx
        .get(param_idx)
        .copied()
        .unwrap_or(-1);
    let active_wave = mod_state
        .driver_waveform_idx
        .get(param_idx)
        .copied()
        .unwrap_or(-1);
    let is_reversed = mod_state
        .driver_reversed
        .get(param_idx)
        .copied()
        .unwrap_or(false);
    let is_dotted = mod_state
        .driver_dotted
        .get(param_idx)
        .copied()
        .unwrap_or(false);
    let is_triplet = mod_state
        .driver_triplet
        .get(param_idx)
        .copied()
        .unwrap_or(false);
    let no_mod = !is_dotted && !is_triplet;

    // Row 1: 11 beat-division buttons, proportional width — fraction labels
    // ("1/32") need more room than integers ("1"), so weighting by label length
    // keeps all 11 on one row without "1/32" clipping.
    let beat_div_buttons: Vec<DrawerButton> = (0..BEAT_DIV_COUNT)
        .map(|j| DrawerButton::new(BEAT_DIV_LABELS[j], j as i32 == active_div && no_mod))
        .collect();

    // Row 2: [.] [T] [Sin] [Tri] [Saw] [Sqr] [Rnd] [Rev] — uniform width.
    // The waveform glyphs are PUA markers U+E000..U+E004 (the UIRenderer draws
    // the SDF waveform icon); dot/triplet/reverse are modifier toggles.
    let mut row2_buttons: Vec<DrawerButton> =
        vec![DrawerButton::new(".", is_dotted), DrawerButton::new("T", is_triplet)];
    for j in 0..WAVEFORM_COUNT {
        let icon_char = char::from_u32(0xE000 + j as u32).unwrap();
        row2_buttons.push(DrawerButton::new(icon_char.to_string(), j as i32 == active_wave));
    }
    row2_buttons.push(DrawerButton::new("Rev", is_reversed));

    let spec = DrawerSpec {
        rows: vec![
            DrawerRow::Buttons { buttons: beat_div_buttons, width: ButtonWidth::Proportional },
            DrawerRow::Buttons { buttons: row2_buttons, width: ButtonWidth::Uniform },
        ],
        btn_font_size,
        slider_font_size: FONT_SIZE,
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec);

    // Reconstruct the typed ids from the flat button list: row 1 is indices
    // 0..BEAT_DIV_COUNT; row 2 is dot, triplet, WAVEFORM_COUNT waves, reverse.
    let ids = dids.button_ids();
    let beat_div_btn_ids: [i32; BEAT_DIV_COUNT] = std::array::from_fn(|j| ids[j]);
    let dot_btn_id = ids[BEAT_DIV_COUNT];
    let triplet_btn_id = ids[BEAT_DIV_COUNT + 1];
    let wave_base = BEAT_DIV_COUNT + 2;
    let wave_btn_ids: [i32; WAVEFORM_COUNT] = std::array::from_fn(|j| ids[wave_base + j]);
    let reverse_btn_id = ids[wave_base + WAVEFORM_COUNT];

    DriverConfigIds {
        _container_id: dids.container,
        beat_div_btn_ids,
        dot_btn_id,
        triplet_btn_id,
        wave_btn_ids,
        reverse_btn_id,
    }
}

/// Orange envelope target handle on a parameter's slider track. Sits at the
/// `target_normalized` position across the track — the depth the envelope pulls
/// the value toward, read in the parameter's own range. Grabbable by feel via
/// the proximity catch-zone in the panel's pointer-down handler.
pub(crate) fn build_envelope_target(
    tree: &mut UITree,
    track_parent: i32,
    track_rect: Rect,
    mod_state: &ParamModState,
    param_idx: usize,
) -> EnvelopeTargetIds {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let norm = mod_state.target_norm.get(param_idx).copied().unwrap_or(0.5);
    let bar_x = track_rect.x + OVERLAY_INSET + norm * usable - TARGET_BAR_W * 0.5;
    let bar_h = track_rect.height + 4.0;
    let bar_y = track_rect.y - 2.0;

    let target_bar_id = tree.add_button(
        track_parent,
        bar_x,
        bar_y,
        TARGET_BAR_W,
        bar_h,
        UIStyle {
            bg_color: color::ENVELOPE_ACTIVE_C32,
            hover_bg_color: color::TARGET_BAR_HOVER_C32,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    EnvelopeTargetIds { target_bar_id }
}

/// The envelope drawer: a single "Decay" slider (`decay_beats`, 0..ENV_DECAY_MAX
/// beats). The one ADSR stage kept — how fast the value falls back after a
/// trigger. Depth is the orange target handle on the track above.
pub(crate) fn build_envelope_config(
    tree: &mut UITree,
    parent: i32,
    x: f32,
    y: f32,
    w: f32,
    mod_state: &ParamModState,
    param_idx: usize,
) -> EnvelopeConfigIds {
    use crate::panels::drawer::{self, DrawerRow, DrawerSpec};

    let decay = mod_state
        .env_decay
        .get(param_idx)
        .copied()
        .unwrap_or(DEFAULT_ENV_DECAY);
    let spec = DrawerSpec {
        rows: vec![DrawerRow::Slider {
            label: "Decay".into(),
            norm: (decay / ENV_DECAY_MAX).clamp(0.0, 1.0),
            value_text: format!("{decay:.2}"),
            colors: SliderColors::envelope(),
            label_w: ENV_DECAY_LABEL_W,
        }],
        btn_font_size: FONT_SIZE,
        slider_font_size: FONT_SIZE,
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec);
    let decay_slider = dids
        .sliders
        .into_iter()
        .next()
        .expect("envelope drawer has one slider row");

    EnvelopeConfigIds {
        _container_id: dids.container,
        decay_slider,
    }
}

pub(crate) fn build_trim_handles(
    tree: &mut UITree,
    track_parent: i32,
    track_rect: Rect,
    mod_state: &ParamModState,
    param_idx: usize,
) -> TrimHandleIds {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let tmin = mod_state.trim_min.get(param_idx).copied().unwrap_or(0.0);
    let tmax = mod_state.trim_max.get(param_idx).copied().unwrap_or(1.0);

    let fill_x = track_rect.x + OVERLAY_INSET + tmin * usable;
    let fill_w = (tmax - tmin) * usable;
    let fill_id = tree.add_panel(
        track_parent,
        fill_x,
        track_rect.y + OVERLAY_INSET,
        fill_w,
        track_rect.height - OVERLAY_INSET * 2.0,
        UIStyle {
            bg_color: color::TRIM_FILL_C32,
            ..UIStyle::default()
        },
    ) as i32;

    let min_x = fill_x - TRIM_BAR_W * 0.5;
    let min_bar_id = tree.add_button(
        track_parent,
        min_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::TRIM_BAR_HOVER_C32,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    let max_x = track_rect.x + OVERLAY_INSET + tmax * usable - TRIM_BAR_W * 0.5;
    let max_bar_id = tree.add_button(
        track_parent,
        max_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::TRIM_BAR_HOVER_C32,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    TrimHandleIds {
        fill_id,
        min_bar_id,
        max_bar_id,
    }
}

/// Build trim handles from explicit min/max values (used by Ableton mappings).
/// Same visual as driver trim handles but with configurable colors.
pub(crate) fn build_trim_handles_explicit(
    tree: &mut UITree,
    track_parent: i32,
    track_rect: Rect,
    min: f32,
    max: f32,
    bar_color: Color32,
    bar_hover: Color32,
    fill_color: Color32,
) -> TrimHandleIds {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;

    let fill_x = track_rect.x + OVERLAY_INSET + min * usable;
    let fill_w = (max - min) * usable;
    let fill_id = tree.add_panel(
        track_parent,
        fill_x,
        track_rect.y + OVERLAY_INSET,
        fill_w,
        track_rect.height - OVERLAY_INSET * 2.0,
        UIStyle {
            bg_color: fill_color,
            ..UIStyle::default()
        },
    ) as i32;

    let min_x = fill_x - TRIM_BAR_W * 0.5;
    let min_bar_id = tree.add_button(
        track_parent,
        min_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: bar_color,
            hover_bg_color: bar_hover,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    let max_x = track_rect.x + OVERLAY_INSET + max * usable - TRIM_BAR_W * 0.5;
    let max_bar_id = tree.add_button(
        track_parent,
        max_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: bar_color,
            hover_bg_color: bar_hover,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    TrimHandleIds {
        fill_id,
        min_bar_id,
        max_bar_id,
    }
}

// ── Shared event helpers ────────────────────────────────────────

/// Result of checking a click against driver config buttons.
pub(crate) enum DriverClickResult {
    BeatDiv(usize),
    Dot,
    Triplet,
    Wave(usize),
    Reverse,
}

/// Check if a click hit any button in a driver config panel.
/// Returns `Some((param_index, result))` if matched.
pub(crate) fn check_driver_config_click(
    node_id: i32,
    driver_config_ids: &[Option<DriverConfigIds>],
) -> Option<(usize, DriverClickResult)> {
    for (pi, cfg) in driver_config_ids.iter().enumerate() {
        if let Some(c) = cfg {
            for (j, &bid) in c.beat_div_btn_ids.iter().enumerate() {
                if node_id == bid {
                    return Some((pi, DriverClickResult::BeatDiv(j)));
                }
            }
            if node_id == c.dot_btn_id {
                return Some((pi, DriverClickResult::Dot));
            }
            if node_id == c.triplet_btn_id {
                return Some((pi, DriverClickResult::Triplet));
            }
            for (j, &wid) in c.wave_btn_ids.iter().enumerate() {
                if node_id == wid {
                    return Some((pi, DriverClickResult::Wave(j)));
                }
            }
            if node_id == c.reverse_btn_id {
                return Some((pi, DriverClickResult::Reverse));
            }
        }
    }
    None
}

// ── Ableton config drawer ───────────────────────────────────────

pub(crate) fn build_ableton_config(
    tree: &mut UITree,
    parent: i32,
    x: f32,
    y: f32,
    w: f32,
    display: &AbletonMappingDisplay,
) -> AbletonConfigIds {
    use crate::panels::drawer::{self, DrawerRow, DrawerSpec, StatusDot, StatusStrip, TrailingButton};

    let dot_color = match display.status {
        AbletonMappingStatus::Active => color::STATUS_DOT_GREEN,
        AbletonMappingStatus::Dormant => color::STATUS_DOT_YELLOW,
        AbletonMappingStatus::Ambiguous => color::STATUS_BAD,
    };

    // Compose the label as "macro_name  ·  track > device" so the user can see
    // the actual stored target rack at a glance. This makes corrupted mappings
    // (where the stored target doesn't match what was originally mapped)
    // immediately visible without changing any routing — the values still flow
    // wherever the resolver landed, but the user can audit it from the card.
    let composite_label = if display.track_name.is_empty() && display.device_name.is_empty() {
        display.macro_name.clone()
    } else {
        format!(
            "{}  ·  {} > {}",
            display.macro_name, display.track_name, display.device_name
        )
    };

    // The strip's row height is the container height minus the drawer's top/
    // bottom pad; the API centers each element within the row, reproducing the
    // original metrics (6px dot, 28×16 INV, 12px label) exactly.
    let spec = DrawerSpec {
        rows: vec![DrawerRow::Status(StatusStrip {
            height: ABL_CONFIG_HEIGHT - drawer::TOP_PAD * 2.0,
            dot: Some(StatusDot { size: 6.0, color: dot_color }),
            label: composite_label,
            label_color: color::TEXT_DIMMED_C32,
            label_font: color::FONT_CAPTION,
            trailing: Some(TrailingButton {
                label: "INV".into(),
                width: 28.0,
                height: 16.0,
                style: config_btn_style_colored(
                    display.inverted,
                    color::ABL_BADGE_C32,
                    color::FONT_CAPTION,
                ),
            }),
        })],
        btn_font_size: color::FONT_CAPTION,
        slider_font_size: FONT_SIZE,
    };
    let dids = drawer::build(tree, parent, x, y, w, &spec);
    let invert_btn_id = dids.button_ids()[0];

    AbletonConfigIds {
        _container_id: dids.container,
        invert_btn_id,
    }
}

/// Check if a click hit an Ableton config button. Returns param index if matched.
pub(crate) fn check_ableton_config_click(
    node_id: i32,
    ableton_config_ids: &[Option<AbletonConfigIds>],
) -> Option<(usize, AbletonConfigClick)> {
    for (pi, ids) in ableton_config_ids.iter().enumerate() {
        if let Some(c) = ids
            && node_id == c.invert_btn_id
        {
            return Some((pi, AbletonConfigClick::Invert));
        }
    }
    None
}

pub(crate) enum AbletonConfigClick {
    Invert,
}

// ── Shared per-parameter slider row ─────────────────────────────────

/// Node IDs produced by [`build_param_row`] for one parameter row. The caller
/// stores each into its parallel per-param vectors at the row's index.
pub(crate) struct ParamRowIds {
    pub(crate) slider: Option<SliderNodeIds>,
    pub(crate) trim: Option<TrimHandleIds>,
    /// Orange envelope target handle on the slider track (when armed).
    pub(crate) target: Option<EnvelopeTargetIds>,
    pub(crate) ableton_trim: Option<TrimHandleIds>,
    /// Green audio-mod trim handles on the slider track (when an audio mod is
    /// armed) — the output sub-range the audio drives.
    pub(crate) audio_trim: Option<TrimHandleIds>,
    pub(crate) envelope_btn: i32,
    pub(crate) driver_btn: i32,
    /// The "A" audio-modulation button (right of the driver button).
    pub(crate) audio_btn: i32,
    /// Envelope drawer (the single "Decay" slider).
    pub(crate) envelope_config: Option<EnvelopeConfigIds>,
    pub(crate) driver_config: Option<DriverConfigIds>,
    pub(crate) ableton_config: Option<AbletonConfigIds>,
    /// Audio-modulation drawer (send + feature selectors) and its send count,
    /// kept so click resolution can split the flat button index into
    /// send / new-send / feature regions.
    pub(crate) audio_config: Option<(crate::panels::drawer::DrawerIds, usize)>,
    /// `y` after this row's slider + any expanded driver/envelope/Ableton
    /// drawers — the caller continues the next row from here.
    pub(crate) new_cy: f32,
}

/// Build one parameter's slider row plus its expanded driver/envelope/Ableton
/// drawers, returning the created node IDs and the post-row `y`.
///
/// This is the per-parameter core shared verbatim by the effect and generator
/// kinds of `ParamCardPanel` — the bulk of what used to be duplicated between
/// the two cards' build paths. The two kinds
/// differ only in the parameters threaded in here: `parent` (the effect card
/// nests rows under its inner-bg panel, the generator card parents flat to
/// `-1`), `slider_colors` (`default_slider` vs `gen_param`), `config_font`
/// (the driver-config button font), and `build_env_button` (effects gate the
/// `E` button on `supports_envelopes`; generators always show it).
///
/// `x` is the row's left edge (already padded); `slider_w` the slider width
/// (track + label, D/E buttons reserved to its right); `config_w` the full
/// inner content width the drawers span. Node creation order is identical to
/// the prior inline code, so first-node/node-count bookkeeping is preserved.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_param_row(
    tree: &mut UITree,
    parent: i32,
    x: f32,
    cy: f32,
    slider_w: f32,
    config_w: f32,
    info: &ParamInfo,
    mod_state: &ParamModState,
    i: usize,
    slider_colors: &SliderColors,
    config_font: u16,
    build_env_button: bool,
    // Width of the right-aligned label cell at the row's left edge. The
    // inspector passes the default; the graph editor's wide lane passes a
    // larger value so friendly names ("Particle Count") don't clip.
    label_width: f32,
) -> ParamRowIds {
    let mut ids = ParamRowIds {
        slider: None,
        trim: None,
        audio_trim: None,
        target: None,
        ableton_trim: None,
        envelope_btn: -1,
        driver_btn: -1,
        audio_btn: -1,
        envelope_config: None,
        driver_config: None,
        ableton_config: None,
        audio_config: None,
        new_cy: cy,
    };
    let mut cy = cy;

    let norm = BitmapSlider::value_to_normalized(info.default, info.min, info.max);
    let val_text = format_param_value(
        info.default,
        info.min,
        info.whole_numbers,
        info.is_angle,
        info.value_labels.as_deref(),
    );
    let slider_rect = Rect::new(x, cy, slider_w, ROW_HEIGHT);
    let slider = BitmapSlider::build(
        tree,
        parent,
        slider_rect,
        Some(&info.name),
        norm,
        &val_text,
        slider_colors,
        FONT_SIZE,
        label_width,
    );

    // Make label interactive for click-to-copy OSC address + Ableton mapping.
    if slider.label >= 0 {
        tree.set_flag(slider.label as u32, UIFlags::INTERACTIVE);
    }

    // Trim handles (if driver expanded).
    if mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
        ids.trim = Some(build_trim_handles(
            tree,
            slider.track as i32,
            slider.track_rect,
            mod_state,
            i,
        ));
    }

    // Envelope target handle on the slider track (when the envelope is armed) —
    // the orange grab bar that sets the depth in the parameter's own range.
    if mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
        ids.target = Some(build_envelope_target(
            tree,
            slider.track as i32,
            slider.track_rect,
            mod_state,
            i,
        ));
    }

    // Ableton trim handles (when the param has an Ableton mapping).
    if let Some((amin, amax)) = info.ableton_range {
        ids.ableton_trim = Some(build_trim_handles_explicit(
            tree,
            slider.track as i32,
            slider.track_rect,
            amin,
            amax,
            color::ABL_TRIM_BAR_C32,
            color::ABL_TRIM_BAR_HOVER_C32,
            color::ABL_TRIM_FILL_C32,
        ));
    }

    // Green audio-mod trim handles (when an audio mod is armed) — the output
    // sub-range the audio drives. Drawn on top of any driver/Ableton handles so
    // all active modulators show their range at once, told apart by color.
    if mod_state.audio_active.get(i).copied().unwrap_or(false) {
        let amin = mod_state.audio_range_min.get(i).copied().unwrap_or(0.0);
        let amax = mod_state.audio_range_max.get(i).copied().unwrap_or(1.0);
        ids.audio_trim = Some(build_trim_handles_explicit(
            tree,
            slider.track as i32,
            slider.track_rect,
            amin,
            amax,
            color::AUDIO_TRIM_BAR_C32,
            color::AUDIO_TRIM_BAR_HOVER_C32,
            color::AUDIO_TRIM_FILL_C32,
        ));
    }

    ids.slider = Some(slider);

    // D/E buttons (right of the slider row).
    let btn_x = x + slider_w + DE_BUTTON_GAP;
    let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
    if build_env_button {
        let env_active = mod_state.envelope_expanded.get(i).copied().unwrap_or(false);
        ids.envelope_btn = tree.add_button(
            parent,
            btn_x,
            btn_y,
            DE_BUTTON_SIZE,
            DE_BUTTON_SIZE,
            de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
            "E",
        ) as i32;
    }
    let drv_active = mod_state.driver_expanded.get(i).copied().unwrap_or(false);
    let drv_btn_x = btn_x
        + if build_env_button {
            DE_BUTTON_SIZE + DE_BUTTON_GAP
        } else {
            0.0
        };
    ids.driver_btn = tree.add_button(
        parent,
        drv_btn_x,
        btn_y,
        DE_BUTTON_SIZE,
        DE_BUTTON_SIZE,
        de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
        "\u{2192}", // →
    ) as i32;

    // Audio-modulation button — third in the lane, right of the driver button.
    let audio_active = mod_state.audio_active.get(i).copied().unwrap_or(false);
    let audio_btn_x = drv_btn_x + DE_BUTTON_SIZE + DE_BUTTON_GAP;
    ids.audio_btn = tree.add_button(
        parent,
        audio_btn_x,
        btn_y,
        DE_BUTTON_SIZE,
        DE_BUTTON_SIZE,
        de_btn_style(audio_active, AUDIO_MOD_ACTIVE_C32),
        "A",
    ) as i32;

    cy += ROW_HEIGHT + ROW_SPACING;

    // Envelope drawer — a single "Decay" slider. Depth is the orange target
    // handle on the track above; this is how fast the value falls back.
    if mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
        ids.envelope_config = Some(build_envelope_config(
            tree, parent, x, cy, config_w, mod_state, i,
        ));
        cy += ENV_CONFIG_HEIGHT;
    }

    // Driver config drawer.
    if mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
        ids.driver_config = Some(build_driver_config(
            tree,
            parent,
            x,
            cy,
            config_w,
            mod_state,
            i,
            config_font,
        ));
        cy += DRIVER_CONFIG_HEIGHT;
    }

    // Ableton config drawer (auto-shows when mapping exists).
    if let Some(ref display) = info.ableton_display {
        ids.ableton_config = Some(build_ableton_config(tree, parent, x, cy, config_w, display));
        cy += ABL_CONFIG_HEIGHT;
    }

    // Audio-modulation drawer — auto-shows while a mod is active (mirrors the
    // driver). Two rows: send selector (the project's sends — new sends are
    // created in the Audio Setup panel, not here) and feature selector. Built
    // on the shared drawer API.
    if audio_active {
        use crate::panels::drawer::{self, ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};
        let send_sel = mod_state.audio_send_idx.get(i).copied().unwrap_or(-1);
        let send_count = mod_state.audio_send_labels.len();
        let send_buttons: Vec<DrawerButton> = mod_state
            .audio_send_labels
            .iter()
            .enumerate()
            .map(|(k, label)| {
                let btn = DrawerButton::new(label.clone(), k as i32 == send_sel);
                // Tint with the send's identity color so a driven slider reads
                // the same color as its source in the Audio Setup panel.
                match mod_state.audio_send_ids.get(k) {
                    Some(id) => btn.with_accent(crate::panels::audio_send_color(id)),
                    None => btn,
                }
            })
            .collect();
        let feat_sel = mod_state.audio_feature_idx.get(i).copied().unwrap_or(0);
        let invert_on = mod_state.audio_invert.get(i).copied().unwrap_or(false);
        let mut feat_buttons: Vec<DrawerButton> = AUDIO_FEATURE_LABELS
            .iter()
            .enumerate()
            .map(|(k, l)| DrawerButton::new(*l, k as i32 == feat_sel))
            .collect();
        // Trailing invert toggle (loud → low). Its flat index sits one past the
        // features, so the click handler reads it as feature == LABELS.len().
        feat_buttons.push(DrawerButton::new("Inv", invert_on));
        let spec = DrawerSpec {
            rows: vec![
                DrawerRow::Buttons { buttons: send_buttons, width: ButtonWidth::Proportional },
                DrawerRow::Buttons { buttons: feat_buttons, width: ButtonWidth::Proportional },
            ],
            btn_font_size: config_font,
            slider_font_size: FONT_SIZE,
        };
        let dids = drawer::build(tree, parent, x, cy, config_w, &spec);
        cy += dids.height;
        ids.audio_config = Some((dids, send_count));
    }

    ids.new_cy = cy;
    ids
}

// ── Shared per-parameter click dispatch ─────────────────────────────

/// A click on one of a parameter row's interactive elements, abstracted away
/// from the effect-vs-generator [`PanelAction`] vocabulary. Each panel maps
/// these to its own kind-specific actions (e.g. `EffectDriverToggle(ei, …)`
/// vs `GenDriverToggle(…)`).
pub(crate) enum RowClick {
    /// The row's `→` driver toggle button (param index).
    DriverToggle(usize),
    /// The row's `E` envelope toggle button (param index).
    EnvelopeToggle(usize),
    /// A button inside the driver-config drawer (param index + action).
    DriverConfig(usize, DriverConfigAction),
    /// The Ableton-config invert button (param index).
    AbletonInvert(usize),
    /// The "A" audio-modulation button (param index) — arm/disarm.
    AudioToggle(usize),
    /// A send button in the audio drawer (param index, send index).
    AudioSelectSend(usize, usize),
    /// A feature button in the audio drawer (param index, feature index).
    AudioSelectFeature(usize, usize),
    /// The "Inv" invert toggle in the audio drawer (param index).
    AudioToggleInvert(usize),
    /// The slider's param label, when it carries an OSC address to copy
    /// (param index). The caller performs the copied-flash side effect and
    /// reads `osc_addresses[pi]`.
    LabelCopy(usize),
}

/// Match a clicked node id against a parameter row's interactive elements,
/// shared by the effect and generator cards' `handle_click`. Returns the
/// abstract [`RowClick`] for the caller to map to a kind-specific action, or
/// `None` if `id` hits nothing in the per-param row surface (the caller then
/// checks its own shell-specific elements — header buttons, toggle/string
/// rows, card selection).
///
/// Driver/envelope toggle buttons on toggle/trigger params are skipped (they
/// carry no slider to modulate). Effects have no toggle/trigger params, so the
/// skip is a no-op there — behavior is identical to the prior per-panel code.
#[allow(clippy::too_many_arguments)]
pub(crate) fn match_param_row_click(
    id: i32,
    driver_btn_ids: &[i32],
    envelope_btn_ids: &[i32],
    driver_config_ids: &[Option<DriverConfigIds>],
    ableton_config_ids: &[Option<AbletonConfigIds>],
    audio_btn_ids: &[i32],
    audio_configs: &[Option<(crate::panels::drawer::DrawerIds, usize)>],
    slider_ids: &[Option<SliderNodeIds>],
    osc_addresses: &[Option<String>],
    param_info: &[ParamInfo],
) -> Option<RowClick> {
    let is_unmodulatable = |pi: usize| {
        param_info
            .get(pi)
            .map(|p| p.is_toggle || p.is_trigger)
            .unwrap_or(false)
    };

    // D/E buttons (skip toggle/trigger params).
    for (pi, &btn_id) in driver_btn_ids.iter().enumerate() {
        if is_unmodulatable(pi) {
            continue;
        }
        if id == btn_id {
            return Some(RowClick::DriverToggle(pi));
        }
    }
    for (pi, &btn_id) in envelope_btn_ids.iter().enumerate() {
        if is_unmodulatable(pi) {
            continue;
        }
        if id == btn_id {
            return Some(RowClick::EnvelopeToggle(pi));
        }
    }

    // Driver config drawer buttons.
    if let Some((pi, result)) = check_driver_config_click(id, driver_config_ids) {
        let action = match result {
            DriverClickResult::BeatDiv(j) => DriverConfigAction::BeatDiv(j),
            DriverClickResult::Dot => DriverConfigAction::Dot,
            DriverClickResult::Triplet => DriverConfigAction::Triplet,
            DriverClickResult::Wave(j) => DriverConfigAction::Wave(j),
            DriverClickResult::Reverse => DriverConfigAction::Reverse,
        };
        return Some(RowClick::DriverConfig(pi, action));
    }

    // Ableton config invert button.
    if let Some((pi, AbletonConfigClick::Invert)) =
        check_ableton_config_click(id, ableton_config_ids)
    {
        return Some(RowClick::AbletonInvert(pi));
    }

    // Audio "A" buttons (skip toggle/trigger params).
    for (pi, &btn_id) in audio_btn_ids.iter().enumerate() {
        if is_unmodulatable(pi) {
            continue;
        }
        if id == btn_id {
            return Some(RowClick::AudioToggle(pi));
        }
    }

    // Audio drawer buttons: flat index splits into send / feature. Sends come
    // first; the feature row (features + a trailing "Inv" toggle) follows.
    for (pi, cfg) in audio_configs.iter().enumerate() {
        if let Some((dids, send_count)) = cfg
            && let Some(flat) = dids.resolve_button(id)
        {
            return Some(if flat < *send_count {
                RowClick::AudioSelectSend(pi, flat)
            } else {
                let feat = flat - send_count;
                if feat == AUDIO_FEATURE_LABELS.len() {
                    RowClick::AudioToggleInvert(pi)
                } else {
                    RowClick::AudioSelectFeature(pi, feat)
                }
            });
        }
    }

    // Slider label → copy OSC address (only when one exists for this slot).
    for (pi, slider) in slider_ids.iter().enumerate() {
        if let Some(ids) = slider
            && ids.label >= 0
            && id == ids.label
            && osc_addresses.get(pi).and_then(|a| a.as_ref()).is_some()
        {
            return Some(RowClick::LabelCopy(pi));
        }
    }

    None
}
