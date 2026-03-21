//! Shared constants, types, and builder functions for parameter slider panels.
//!
//! Both `EffectCardPanel` and `GenParamPanel` use identical layout constants,
//! driver/envelope config builders, trim/target handle builders, and formatting
//! helpers. This module extracts them into a single source of truth.

use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;

// ── Shared layout constants ─────────────────────────────────────

pub(crate) const ROW_HEIGHT: f32 = 20.0;
pub(crate) const ROW_SPACING: f32 = 2.0;
pub(crate) const PADDING: f32 = 4.0;
pub(crate) const GAP: f32 = 4.0;
pub(crate) const FONT_SIZE: u16 = 10;

pub(crate) const DE_BUTTON_SIZE: f32 = 20.0;
pub(crate) const DE_BUTTON_GAP: f32 = 2.0;

pub(crate) const DRIVER_CONFIG_HEIGHT: f32 = 52.0;
pub(crate) const DRIVER_ROW_HEIGHT: f32 = 22.0;
#[allow(dead_code)]
pub(crate) const BEAT_DIV_BTN_W: f32 = 27.0;
pub(crate) const BEAT_DIV_SPACING: f32 = 1.0;
pub(crate) const WAVE_BTN_W: f32 = 30.0;
pub(crate) const DRIVER_PAD_H: f32 = 5.0;
pub(crate) const BEAT_DIV_COUNT: usize = 11;
pub(crate) const WAVEFORM_COUNT: usize = 5;

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
    "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32", "64",
];

pub(crate) const WAVEFORM_LABELS: [&str; WAVEFORM_COUNT] = ["Sin", "Tri", "Saw", "Sqr", "Rnd"];

// ── Shared node ID structs ──────────────────────────────────────

pub(crate) struct DriverConfigIds {
    pub(crate) container_id: i32,
    pub(crate) beat_div_btn_ids: [i32; BEAT_DIV_COUNT],
    pub(crate) dot_btn_id: i32,
    pub(crate) triplet_btn_id: i32,
    pub(crate) wave_btn_ids: [i32; WAVEFORM_COUNT],
    pub(crate) reverse_btn_id: i32,
}

pub(crate) struct EnvelopeConfigIds {
    pub(crate) container_id: i32,
    pub(crate) attack_slider: SliderNodeIds,
    pub(crate) decay_slider: SliderNodeIds,
    pub(crate) sustain_slider: SliderNodeIds,
    pub(crate) release_slider: SliderNodeIds,
}

pub(crate) struct TrimHandleIds {
    pub(crate) fill_id: i32,
    pub(crate) min_bar_id: i32,
    pub(crate) max_bar_id: i32,
}

pub(crate) struct EnvelopeTargetIds {
    pub(crate) target_bar_id: i32,
}

// ── Shared modulation state ─────────────────────────────────────

/// Per-parameter modulation state shared by both EffectCardPanel and GenParamPanel.
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
            env_attack: vec![0.1; param_count],
            env_decay: vec![0.3; param_count],
            env_sustain: vec![0.7; param_count],
            env_release: vec![0.5; param_count],
            driver_beat_div_idx: vec![-1; param_count],
            driver_waveform_idx: vec![-1; param_count],
            driver_reversed: vec![false; param_count],
            driver_dotted: vec![false; param_count],
            driver_triplet: vec![false; param_count],
        }
    }

    /// Sync driver/envelope/trim/target/ADSR state from config vectors.
    /// `n` is the param count. Reads from config slices with fallback defaults.
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
            self.driver_beat_div_idx[i] = driver_beat_div_idx.get(i).copied().unwrap_or(-1);
            self.driver_waveform_idx[i] = driver_waveform_idx.get(i).copied().unwrap_or(-1);
            self.driver_reversed[i] = driver_reversed.get(i).copied().unwrap_or(false);
            self.driver_dotted[i] = driver_dotted.get(i).copied().unwrap_or(false);
            self.driver_triplet[i] = driver_triplet.get(i).copied().unwrap_or(false);
        }
    }
}

// ── Shared drag state ───────────────────────────────────────────

/// Drag tracking state shared by both EffectCardPanel and GenParamPanel.
pub(crate) struct ParamDragState {
    pub(crate) dragging_param: i32,
    pub(crate) dragging_env_param: i32,
    pub(crate) dragging_env_slot: usize,
    pub(crate) dragging_trim_param: i32,
    pub(crate) dragging_trim_is_min: bool,
    pub(crate) dragging_target_param: i32,
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
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.dragging_param >= 0 || self.dragging_env_param >= 0
            || self.dragging_trim_param >= 0 || self.dragging_target_param >= 0
    }
}

// ── Shared helper functions ─────────────────────────────────────

pub(crate) fn format_param_value(val: f32, whole_numbers: bool, value_labels: Option<&[String]>) -> String {
    if let Some(labels) = value_labels {
        let idx = (val.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
        return labels[idx].clone();
    }
    if whole_numbers { format!("{}", val.round() as i32) } else { format!("{:.2}", val) }
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
            font_size: 8,
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
            font_size: 8,
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

pub(crate) fn toggle_btn_style(enabled: bool) -> UIStyle {
    if enabled {
        UIStyle {
            bg_color: color::ACCENT_BLUE_C32,
            hover_bg_color: color::ACCENT_BLUE_HOVER_C32,
            pressed_bg_color: color::ACCENT_BLUE_PRESS_C32,
            text_color: color::TEXT_WHITE_C32,
            font_size: 8,
            font_weight: FontWeight::Bold,
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
            font_size: 8,
            font_weight: FontWeight::Bold,
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
        parent, x, y, w, DRIVER_CONFIG_HEIGHT,
        UIStyle { bg_color: color::CONFIG_BG_C32, corner_radius: 2.0, ..UIStyle::default() },
    ) as i32;

    let active_div = mod_state.driver_beat_div_idx.get(param_idx).copied().unwrap_or(-1);
    let active_wave = mod_state.driver_waveform_idx.get(param_idx).copied().unwrap_or(-1);
    let is_reversed = mod_state.driver_reversed.get(param_idx).copied().unwrap_or(false);
    let is_dotted = mod_state.driver_dotted.get(param_idx).copied().unwrap_or(false);
    let is_triplet = mod_state.driver_triplet.get(param_idx).copied().unwrap_or(false);
    let no_mod = !is_dotted && !is_triplet;

    let mut cx = x + DRIVER_PAD_H;
    let row1_y = y + 4.0;
    let avail_w = w - DRIVER_PAD_H * 2.0;
    let btn_w = (avail_w - BEAT_DIV_SPACING * (BEAT_DIV_COUNT as f32 - 1.0)) / BEAT_DIV_COUNT as f32;

    let mut beat_div_btn_ids = [-1i32; BEAT_DIV_COUNT];
    for j in 0..BEAT_DIV_COUNT {
        let active = j as i32 == active_div && no_mod;
        beat_div_btn_ids[j] = tree.add_button(
            container_id, cx, row1_y, btn_w, DRIVER_ROW_HEIGHT,
            config_btn_style(active, btn_font_size),
            BEAT_DIV_LABELS[j],
        ) as i32;
        cx += btn_w + BEAT_DIV_SPACING;
    }

    let row2_y = row1_y + DRIVER_ROW_HEIGHT + 4.0;
    cx = x + DRIVER_PAD_H;

    let dot_btn_id = tree.add_button(
        container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
        config_btn_style(is_dotted, btn_font_size), ".",
    ) as i32;
    cx += WAVE_BTN_W + BEAT_DIV_SPACING;

    let triplet_btn_id = tree.add_button(
        container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
        config_btn_style(is_triplet, btn_font_size), "T",
    ) as i32;
    cx += WAVE_BTN_W + GAP;

    let mut wave_btn_ids = [-1i32; WAVEFORM_COUNT];
    for j in 0..WAVEFORM_COUNT {
        let active = j as i32 == active_wave;
        wave_btn_ids[j] = tree.add_button(
            container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
            config_btn_style(active, btn_font_size), WAVEFORM_LABELS[j],
        ) as i32;
        cx += WAVE_BTN_W + BEAT_DIV_SPACING;
    }

    let reverse_w = 32.0;
    let reverse_x = x + w - DRIVER_PAD_H - reverse_w;
    let reverse_btn_id = tree.add_button(
        container_id, reverse_x, row2_y, reverse_w, DRIVER_ROW_HEIGHT,
        config_btn_style(is_reversed, btn_font_size), "Rev",
    ) as i32;

    DriverConfigIds {
        container_id,
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
        parent, x, y, w, ENV_CONFIG_HEIGHT,
        UIStyle { bg_color: color::CONFIG_BG_C32, corner_radius: 2.0, ..UIStyle::default() },
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
        tree, container_id,
        Rect::new(sx, row1_y, half_w, ENV_ROW_HEIGHT),
        Some("A"), attack_val / ENV_ADR_MAX,
        &format!("{:.2}", attack_val), &env_colors, FONT_SIZE, ENV_LABEL_W,
    );

    let decay_slider = BitmapSlider::build(
        tree, container_id,
        Rect::new(sx + half_w + GAP, row1_y, half_w, ENV_ROW_HEIGHT),
        Some("D"), decay_val / ENV_ADR_MAX,
        &format!("{:.2}", decay_val), &env_colors, FONT_SIZE, ENV_LABEL_W,
    );

    let sustain_slider = BitmapSlider::build(
        tree, container_id,
        Rect::new(sx, row2_y, half_w, ENV_ROW_HEIGHT),
        Some("S"), sustain_val / ENV_S_MAX,
        &format!("{:.2}", sustain_val), &env_colors, FONT_SIZE, ENV_LABEL_W,
    );

    let release_slider = BitmapSlider::build(
        tree, container_id,
        Rect::new(sx + half_w + GAP, row2_y, half_w, ENV_ROW_HEIGHT),
        Some("R"), release_val / ENV_ADR_MAX,
        &format!("{:.2}", release_val), &env_colors, FONT_SIZE, ENV_LABEL_W,
    );

    EnvelopeConfigIds {
        container_id,
        attack_slider,
        decay_slider,
        sustain_slider,
        release_slider,
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
        track_parent, fill_x, track_rect.y + OVERLAY_INSET,
        fill_w, track_rect.height - OVERLAY_INSET * 2.0,
        UIStyle { bg_color: color::TRIM_FILL_C32, ..UIStyle::default() },
    ) as i32;

    let min_x = fill_x - TRIM_BAR_W * 0.5;
    let min_bar_id = tree.add_button(
        track_parent, min_x, track_rect.y, TRIM_BAR_W, track_rect.height,
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
        track_parent, max_x, track_rect.y, TRIM_BAR_W, track_rect.height,
        UIStyle {
            bg_color: color::DRIVER_ACTIVE_C32,
            hover_bg_color: color::TRIM_BAR_HOVER_C32,
            corner_radius: 1.0,
            ..UIStyle::default()
        },
        "",
    ) as i32;

    TrimHandleIds { fill_id, min_bar_id, max_bar_id }
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
        track_parent, bar_x, bar_y, TARGET_BAR_W, bar_h,
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
        if let Some(ref c) = cfg {
            for (j, &bid) in c.beat_div_btn_ids.iter().enumerate() {
                if node_id == bid { return Some((pi, DriverClickResult::BeatDiv(j))); }
            }
            if node_id == c.dot_btn_id { return Some((pi, DriverClickResult::Dot)); }
            if node_id == c.triplet_btn_id { return Some((pi, DriverClickResult::Triplet)); }
            for (j, &wid) in c.wave_btn_ids.iter().enumerate() {
                if node_id == wid { return Some((pi, DriverClickResult::Wave(j))); }
            }
            if node_id == c.reverse_btn_id { return Some((pi, DriverClickResult::Reverse)); }
        }
    }
    None
}
