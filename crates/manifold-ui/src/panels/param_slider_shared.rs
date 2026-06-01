//! Shared constants, types, and builder functions for parameter slider panels.
//!
//! The unified `ParamCardPanel` (effect + generator kinds) uses identical
//! layout constants, driver/envelope config builders, trim/target handle
//! builders, and formatting helpers across both kinds. This module is the
//! single source of truth for them.

use super::DriverConfigAction;
use super::param_card::ParamInfo;
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
pub use manifold_core::ableton_mapping::AbletonMappingStatus;
pub use manifold_core::effects::EnvelopeMode;

// ── Shared layout constants ─────────────────────────────────────

pub(crate) const ROW_HEIGHT: f32 = 20.0;
pub(crate) const ROW_SPACING: f32 = 4.0;
pub(crate) const PADDING: f32 = 6.0;
pub(crate) const GAP: f32 = 4.0;
pub(crate) const FONT_SIZE: u16 = color::FONT_BODY;

pub(crate) const DE_BUTTON_SIZE: f32 = 20.0;
pub(crate) const DE_BUTTON_GAP: f32 = 2.0;

pub(crate) const DRIVER_CONFIG_HEIGHT: f32 = 56.0;
pub(crate) const DRIVER_ROW_HEIGHT: f32 = 22.0;
pub(crate) const BEAT_DIV_SPACING: f32 = 1.0;
pub(crate) const DRIVER_PAD_H: f32 = 5.0;
pub(crate) const BEAT_DIV_COUNT: usize = 11;
pub(crate) const WAVEFORM_COUNT: usize = 5;

pub(crate) const ABL_CONFIG_HEIGHT: f32 = 24.0;

pub(crate) const ENV_CONFIG_HEIGHT: f32 = 55.0;
pub(crate) const ENV_ROW_HEIGHT: f32 = 22.0;
pub(crate) const ENV_LABEL_W: f32 = 17.0;
pub(crate) const ENV_PAD_H: f32 = 5.0;

pub(crate) const TRIM_BAR_W: f32 = 4.0;
pub(crate) const TARGET_BAR_W: f32 = 6.0;
pub(crate) const OVERLAY_INSET: f32 = 1.0;

pub(crate) const ENV_ADR_MAX: f32 = 8.0;
pub(crate) const ENV_S_MAX: f32 = 1.0;

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

pub(crate) struct EnvelopeConfigIds {
    pub(crate) _container_id: i32,
    pub(crate) attack_slider: SliderNodeIds,
    pub(crate) decay_slider: SliderNodeIds,
    pub(crate) sustain_slider: SliderNodeIds,
    pub(crate) release_slider: SliderNodeIds,
}

pub(crate) struct EnvelopeRandomConfigIds {
    pub(crate) _container_id: i32,
    pub(crate) mode_btn_id: i32,
    pub(crate) jump_btn_id: i32,
}

pub(crate) struct TrimHandleIds {
    pub(crate) fill_id: i32,
    pub(crate) min_bar_id: i32,
    pub(crate) max_bar_id: i32,
}

pub(crate) struct EnvelopeTargetIds {
    pub(crate) target_bar_id: i32,
}

pub(crate) struct AbletonConfigIds {
    pub(crate) _container_id: i32,
    pub(crate) _status_dot_id: i32,
    pub(crate) _macro_label_id: i32,
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
/// Contains driver expansion, envelope expansion, trim/target values, ADSR values,
/// and driver visual state (beat div, waveform, reversed, dotted, triplet).
pub struct ParamModState {
    pub driver_expanded: Vec<bool>,
    pub envelope_expanded: Vec<bool>,
    pub trim_min: Vec<f32>,
    pub trim_max: Vec<f32>,
    pub target_norm: Vec<f32>,
    pub env_attack: Vec<f32>,
    pub env_decay: Vec<f32>,
    pub env_sustain: Vec<f32>,
    pub env_release: Vec<f32>,
    pub env_mode: Vec<EnvelopeMode>,
    pub env_random_jump: Vec<bool>,
    pub env_range_min: Vec<f32>,
    pub env_range_max: Vec<f32>,
    pub driver_beat_div_idx: Vec<i32>,
    pub driver_waveform_idx: Vec<i32>,
    pub driver_reversed: Vec<bool>,
    pub driver_dotted: Vec<bool>,
    pub driver_triplet: Vec<bool>,
}

impl ParamModState {
    pub fn allocate(param_count: usize) -> Self {
        Self {
            driver_expanded: vec![false; param_count],
            envelope_expanded: vec![false; param_count],
            trim_min: vec![0.0; param_count],
            trim_max: vec![1.0; param_count],
            target_norm: vec![0.5; param_count],
            env_attack: vec![0.0; param_count],
            env_decay: vec![0.0; param_count],
            env_sustain: vec![0.0; param_count],
            env_release: vec![0.0; param_count],
            env_mode: vec![EnvelopeMode::Adsr; param_count],
            env_random_jump: vec![false; param_count],
            env_range_min: vec![0.0; param_count],
            env_range_max: vec![1.0; param_count],
            driver_beat_div_idx: vec![-1; param_count],
            driver_waveform_idx: vec![-1; param_count],
            driver_reversed: vec![false; param_count],
            driver_dotted: vec![false; param_count],
            driver_triplet: vec![false; param_count],
        }
    }

    /// Sync driver/envelope/trim/target/ADSR state from config vectors.
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
        env_attack: &[f32],
        env_decay: &[f32],
        env_sustain: &[f32],
        env_release: &[f32],
        env_mode: &[EnvelopeMode],
        env_random_jump: &[bool],
        env_range_min: &[f32],
        env_range_max: &[f32],
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
            self.env_attack[i] = env_attack.get(i).copied().unwrap_or(0.0);
            self.env_decay[i] = env_decay.get(i).copied().unwrap_or(0.0);
            self.env_sustain[i] = env_sustain.get(i).copied().unwrap_or(0.0);
            self.env_release[i] = env_release.get(i).copied().unwrap_or(0.0);
            self.env_mode[i] = env_mode.get(i).copied().unwrap_or(EnvelopeMode::Adsr);
            self.env_random_jump[i] = env_random_jump.get(i).copied().unwrap_or(false);
            self.env_range_min[i] = env_range_min.get(i).copied().unwrap_or(0.0);
            self.env_range_max[i] = env_range_max.get(i).copied().unwrap_or(1.0);
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
    pub(crate) dragging_env_param: i32,
    pub(crate) dragging_env_slot: usize,
    pub(crate) dragging_trim_param: i32,
    pub(crate) dragging_trim_is_min: bool,
    pub(crate) dragging_target_param: i32,
    pub(crate) dragging_range_param: i32,
    pub(crate) dragging_range_is_min: bool,
    pub(crate) dragging_ableton_trim_param: i32,
    pub(crate) dragging_ableton_trim_is_min: bool,
}

impl ParamDragState {
    pub(crate) fn new() -> Self {
        Self {
            dragging_param: -1,
            dragging_env_param: -1,
            dragging_env_slot: 0,
            dragging_trim_param: -1,
            dragging_trim_is_min: false,
            dragging_target_param: -1,
            dragging_range_param: -1,
            dragging_range_is_min: false,
            dragging_ableton_trim_param: -1,
            dragging_ableton_trim_is_min: false,
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging_param >= 0
            || self.dragging_env_param >= 0
            || self.dragging_trim_param >= 0
            || self.dragging_target_param >= 0
            || self.dragging_range_param >= 0
            || self.dragging_ableton_trim_param >= 0
    }
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
    let container_id = tree.add_panel(
        parent,
        x,
        y,
        w,
        DRIVER_CONFIG_HEIGHT,
        UIStyle {
            bg_color: color::CONFIG_BG_C32,
            corner_radius: 2.0,
            ..UIStyle::default()
        },
    ) as i32;

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

    let mut cx = x + DRIVER_PAD_H;
    let row1_y = y + 4.0;
    let avail_w = w - DRIVER_PAD_H * 2.0;
    let btn_w =
        (avail_w - BEAT_DIV_SPACING * (BEAT_DIV_COUNT as f32 - 1.0)) / BEAT_DIV_COUNT as f32;

    let mut beat_div_btn_ids = [-1i32; BEAT_DIV_COUNT];
    for j in 0..BEAT_DIV_COUNT {
        let active = j as i32 == active_div && no_mod;
        beat_div_btn_ids[j] = tree.add_button(
            container_id,
            cx,
            row1_y,
            btn_w,
            DRIVER_ROW_HEIGHT,
            config_btn_style(active, btn_font_size),
            BEAT_DIV_LABELS[j],
        ) as i32;
        cx += btn_w + BEAT_DIV_SPACING;
    }

    let row2_y = row1_y + DRIVER_ROW_HEIGHT + 4.0;
    cx = x + DRIVER_PAD_H;

    // Row 2: [.] [T] [Sin] [Tri] [Saw] [Sqr] [Rnd] [Rev]
    // 8 buttons total, proportional width like beat divs
    let row2_count = 2 + WAVEFORM_COUNT + 1; // dot, triplet, 5 waveforms, rev
    let wave_btn_w = (avail_w - BEAT_DIV_SPACING * (row2_count as f32 - 1.0)) / row2_count as f32;

    let dot_btn_id = tree.add_button(
        container_id,
        cx,
        row2_y,
        wave_btn_w,
        DRIVER_ROW_HEIGHT,
        config_btn_style(is_dotted, btn_font_size),
        ".",
    ) as i32;
    cx += wave_btn_w + BEAT_DIV_SPACING;

    let triplet_btn_id = tree.add_button(
        container_id,
        cx,
        row2_y,
        wave_btn_w,
        DRIVER_ROW_HEIGHT,
        config_btn_style(is_triplet, btn_font_size),
        "T",
    ) as i32;
    cx += wave_btn_w + BEAT_DIV_SPACING;

    let mut wave_btn_ids = [-1i32; WAVEFORM_COUNT];
    for (j, btn_id) in wave_btn_ids.iter_mut().enumerate() {
        let active = j as i32 == active_wave;
        let style = config_btn_style(active, btn_font_size);
        // PUA marker U+E000..U+E004 — UIRenderer draws the SDF waveform icon
        let icon_char = char::from_u32(0xE000 + j as u32).unwrap();
        let mut icon_text = String::new();
        icon_text.push(icon_char);
        *btn_id = tree.add_button(
            container_id,
            cx,
            row2_y,
            wave_btn_w,
            DRIVER_ROW_HEIGHT,
            style,
            &icon_text,
        ) as i32;
        cx += wave_btn_w + BEAT_DIV_SPACING;
    }

    let reverse_btn_id = tree.add_button(
        container_id,
        cx,
        row2_y,
        wave_btn_w,
        DRIVER_ROW_HEIGHT,
        config_btn_style(is_reversed, btn_font_size),
        "Rev",
    ) as i32;

    DriverConfigIds {
        _container_id: container_id,
        beat_div_btn_ids,
        dot_btn_id,
        triplet_btn_id,
        wave_btn_ids,
        reverse_btn_id,
    }
}

pub(crate) fn build_envelope_config(
    tree: &mut UITree,
    parent: i32,
    x: f32,
    y: f32,
    w: f32,
    mod_state: &ParamModState,
    param_idx: usize,
) -> EnvelopeConfigIds {
    let container_id = tree.add_panel(
        parent,
        x,
        y,
        w,
        ENV_CONFIG_HEIGHT,
        UIStyle {
            bg_color: color::CONFIG_BG_C32,
            corner_radius: 2.0,
            ..UIStyle::default()
        },
    ) as i32;

    let half_w = (w - ENV_PAD_H * 2.0 - GAP) * 0.5;
    let sx = x + ENV_PAD_H;
    let row1_y = y + 4.0;
    let row2_y = row1_y + ENV_ROW_HEIGHT + 4.0;

    let attack_val = mod_state.env_attack.get(param_idx).copied().unwrap_or(0.1);
    let decay_val = mod_state.env_decay.get(param_idx).copied().unwrap_or(0.3);
    let sustain_val = mod_state.env_sustain.get(param_idx).copied().unwrap_or(0.7);
    let release_val = mod_state.env_release.get(param_idx).copied().unwrap_or(0.5);

    let env_colors = SliderColors::envelope();

    let attack_slider = BitmapSlider::build(
        tree,
        container_id,
        Rect::new(sx, row1_y, half_w, ENV_ROW_HEIGHT),
        Some("A"),
        attack_val / ENV_ADR_MAX,
        &format!("{:.2}", attack_val),
        &env_colors,
        FONT_SIZE,
        ENV_LABEL_W,
    );

    let decay_slider = BitmapSlider::build(
        tree,
        container_id,
        Rect::new(sx + half_w + GAP, row1_y, half_w, ENV_ROW_HEIGHT),
        Some("D"),
        decay_val / ENV_ADR_MAX,
        &format!("{:.2}", decay_val),
        &env_colors,
        FONT_SIZE,
        ENV_LABEL_W,
    );

    let sustain_slider = BitmapSlider::build(
        tree,
        container_id,
        Rect::new(sx, row2_y, half_w, ENV_ROW_HEIGHT),
        Some("S"),
        sustain_val / ENV_S_MAX,
        &format!("{:.2}", sustain_val),
        &env_colors,
        FONT_SIZE,
        ENV_LABEL_W,
    );

    let release_slider = BitmapSlider::build(
        tree,
        container_id,
        Rect::new(sx + half_w + GAP, row2_y, half_w, ENV_ROW_HEIGHT),
        Some("R"),
        release_val / ENV_ADR_MAX,
        &format!("{:.2}", release_val),
        &env_colors,
        FONT_SIZE,
        ENV_LABEL_W,
    );

    EnvelopeConfigIds {
        _container_id: container_id,
        attack_slider,
        decay_slider,
        sustain_slider,
        release_slider,
    }
}

/// Height for the random envelope config panel (single row with mode + jump buttons).
pub(crate) const ENV_RANDOM_CONFIG_HEIGHT: f32 = 30.0;

pub(crate) fn build_envelope_random_config(
    tree: &mut UITree,
    parent: i32,
    x: f32,
    y: f32,
    w: f32,
    mod_state: &ParamModState,
    param_idx: usize,
) -> EnvelopeRandomConfigIds {
    let container_id = tree.add_panel(
        parent,
        x,
        y,
        w,
        ENV_RANDOM_CONFIG_HEIGHT,
        UIStyle {
            bg_color: color::CONFIG_BG_C32,
            corner_radius: 2.0,
            ..UIStyle::default()
        },
    ) as i32;

    let is_random = mod_state
        .env_mode
        .get(param_idx)
        .copied()
        .unwrap_or(EnvelopeMode::Adsr)
        == EnvelopeMode::Random;
    let is_jump = mod_state
        .env_random_jump
        .get(param_idx)
        .copied()
        .unwrap_or(false);

    let sx = x + ENV_PAD_H;
    let btn_y = y + 4.0;
    let btn_h = ENV_ROW_HEIGHT;
    let btn_w = 50.0;
    let btn_gap = 4.0;

    // "RND" button — toggles envelope mode between ADSR and Random
    let mode_btn_id = tree.add_button(
        container_id,
        sx,
        btn_y,
        btn_w,
        btn_h,
        config_btn_style(is_random, color::FONT_CAPTION),
        "RND",
    ) as i32;

    // "JUMP" button — toggles random_jump (only meaningful when mode=Random)
    let jump_btn_id = tree.add_button(
        container_id,
        sx + btn_w + btn_gap,
        btn_y,
        btn_w,
        btn_h,
        config_btn_style(is_random && is_jump, color::FONT_CAPTION),
        "JUMP",
    ) as i32;

    EnvelopeRandomConfigIds {
        _container_id: container_id,
        mode_btn_id,
        jump_btn_id,
    }
}

/// Orange range handles for Random envelope mode. Same layout as trim handles
/// but reads from `env_range_min/max` and uses envelope orange colors.
pub(crate) fn build_envelope_range_handles(
    tree: &mut UITree,
    track_parent: i32,
    track_rect: Rect,
    mod_state: &ParamModState,
    param_idx: usize,
) -> TrimHandleIds {
    let usable = track_rect.width - OVERLAY_INSET * 2.0;
    let rmin = mod_state
        .env_range_min
        .get(param_idx)
        .copied()
        .unwrap_or(0.0);
    let rmax = mod_state
        .env_range_max
        .get(param_idx)
        .copied()
        .unwrap_or(1.0);

    let fill_x = track_rect.x + OVERLAY_INSET + rmin * usable;
    let fill_w = (rmax - rmin) * usable;
    let fill_id = tree.add_panel(
        track_parent,
        fill_x,
        track_rect.y + OVERLAY_INSET,
        fill_w,
        track_rect.height - OVERLAY_INSET * 2.0,
        UIStyle {
            bg_color: color::ENV_FILL_C32,
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
            bg_color: color::ENVELOPE_ACTIVE_C32,
            hover_bg_color: color::TARGET_BAR_HOVER_C32,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    let max_x = track_rect.x + OVERLAY_INSET + rmax * usable - TRIM_BAR_W * 0.5;
    let max_bar_id = tree.add_button(
        track_parent,
        max_x,
        track_rect.y,
        TRIM_BAR_W,
        track_rect.height,
        UIStyle {
            bg_color: color::ENVELOPE_ACTIVE_C32,
            hover_bg_color: color::TARGET_BAR_HOVER_C32,
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
    let container_id = tree.add_panel(
        parent,
        x,
        y,
        w,
        ABL_CONFIG_HEIGHT,
        UIStyle {
            bg_color: color::CONFIG_BG_C32,
            corner_radius: 2.0,
            ..UIStyle::default()
        },
    ) as i32;

    let dot_size = 6.0_f32;
    let pad = 6.0_f32;
    let dot_y = y + (ABL_CONFIG_HEIGHT - dot_size) * 0.5;
    let dot_color = match display.status {
        AbletonMappingStatus::Active => color::STATUS_DOT_GREEN,
        AbletonMappingStatus::Dormant => color::STATUS_DOT_YELLOW,
        AbletonMappingStatus::Ambiguous => color::STATUS_BAD,
    };
    let status_dot_id = tree.add_panel(
        container_id,
        x + pad,
        dot_y,
        dot_size,
        dot_size,
        UIStyle {
            bg_color: dot_color,
            corner_radius: dot_size * 0.5,
            ..UIStyle::default()
        },
    ) as i32;

    // INV button (right-aligned)
    let inv_btn_w = 28.0_f32;
    let inv_btn_h = 16.0_f32;
    let inv_btn_x = x + w - pad - inv_btn_w;
    let inv_btn_y = y + (ABL_CONFIG_HEIGHT - inv_btn_h) * 0.5;
    let invert_btn_id = tree.add_button(
        container_id,
        inv_btn_x,
        inv_btn_y,
        inv_btn_w,
        inv_btn_h,
        config_btn_style_colored(display.inverted, color::ABL_BADGE_C32, color::FONT_CAPTION),
        "INV",
    ) as i32;

    // Compose the label as "macro_name  ·  track > device" so the user
    // can see the actual stored target rack at a glance. This makes
    // corrupted mappings (where the stored target doesn't match what
    // was originally mapped) immediately visible without changing any
    // routing — the values still flow wherever the resolver landed,
    // but the user can audit it from the effect card.
    let composite_label = if display.track_name.is_empty() && display.device_name.is_empty() {
        display.macro_name.clone()
    } else {
        format!(
            "{}  ·  {} > {}",
            display.macro_name, display.track_name, display.device_name
        )
    };
    let label_x = x + pad + dot_size + 4.0;
    let label_y = y + (ABL_CONFIG_HEIGHT - 12.0) * 0.5;
    let label_w = inv_btn_x - label_x - 4.0;
    let macro_label_id = tree.add_label(
        container_id,
        label_x,
        label_y,
        label_w,
        12.0,
        &composite_label,
        UIStyle {
            text_color: color::TEXT_DIMMED_C32,
            font_size: color::FONT_CAPTION,
            text_align: TextAlign::Left,
            ..UIStyle::default()
        },
    ) as i32;

    AbletonConfigIds {
        _container_id: container_id,
        _status_dot_id: status_dot_id,
        _macro_label_id: macro_label_id,
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
    pub(crate) target: Option<EnvelopeTargetIds>,
    pub(crate) envelope_range: Option<TrimHandleIds>,
    pub(crate) ableton_trim: Option<TrimHandleIds>,
    pub(crate) envelope_btn: i32,
    pub(crate) driver_btn: i32,
    pub(crate) envelope_config: Option<EnvelopeConfigIds>,
    pub(crate) envelope_random_config: Option<EnvelopeRandomConfigIds>,
    pub(crate) driver_config: Option<DriverConfigIds>,
    pub(crate) ableton_config: Option<AbletonConfigIds>,
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
        target: None,
        envelope_range: None,
        ableton_trim: None,
        envelope_btn: -1,
        driver_btn: -1,
        envelope_config: None,
        envelope_random_config: None,
        driver_config: None,
        ableton_config: None,
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

    // Envelope target or range handles (if envelope expanded).
    if mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
        let env_mode = mod_state
            .env_mode
            .get(i)
            .copied()
            .unwrap_or(EnvelopeMode::Adsr);
        if env_mode == EnvelopeMode::Random {
            ids.envelope_range = Some(build_envelope_range_handles(
                tree,
                slider.track as i32,
                slider.track_rect,
                mod_state,
                i,
            ));
        } else {
            ids.target = Some(build_envelope_target(
                tree,
                slider.track as i32,
                slider.track_rect,
                mod_state,
                i,
            ));
        }
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

    cy += ROW_HEIGHT + ROW_SPACING;

    // Envelope config drawer.
    if mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
        let env_mode = mod_state
            .env_mode
            .get(i)
            .copied()
            .unwrap_or(EnvelopeMode::Adsr);
        // Always build the random config buttons (mode toggle + jump toggle).
        ids.envelope_random_config = Some(build_envelope_random_config(
            tree, parent, x, cy, config_w, mod_state, i,
        ));
        cy += ENV_RANDOM_CONFIG_HEIGHT;
        // ADSR sliders only in ADSR mode.
        if env_mode == EnvelopeMode::Adsr {
            ids.envelope_config = Some(build_envelope_config(
                tree, parent, x, cy, config_w, mod_state, i,
            ));
            cy += ENV_CONFIG_HEIGHT;
        }
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
    /// The envelope mode (ADSR/Random) toggle (param index).
    EnvModeToggle(usize),
    /// The envelope random-jump toggle (param index).
    EnvRandomJumpToggle(usize),
    /// The Ableton-config invert button (param index).
    AbletonInvert(usize),
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
    envelope_random_config_ids: &[Option<EnvelopeRandomConfigIds>],
    ableton_config_ids: &[Option<AbletonConfigIds>],
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

    // Envelope random-config buttons (mode toggle, jump toggle).
    for (pi, cfg) in envelope_random_config_ids.iter().enumerate() {
        if let Some(c) = cfg {
            if id == c.mode_btn_id {
                return Some(RowClick::EnvModeToggle(pi));
            }
            if id == c.jump_btn_id {
                return Some(RowClick::EnvRandomJumpToggle(pi));
            }
        }
    }

    // Ableton config invert button.
    if let Some((pi, AbletonConfigClick::Invert)) =
        check_ableton_config_click(id, ableton_config_ids)
    {
        return Some(RowClick::AbletonInvert(pi));
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
