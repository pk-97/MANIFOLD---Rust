use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::{PanelAction, DriverConfigAction, EnvelopeParam};

// ── Layout constants (from EffectCardBitmapPanel.cs) ──────────────

const HEADER_HEIGHT: f32 = 27.5;
const ROW_HEIGHT: f32 = 20.0;
const ROW_SPACING: f32 = 2.0;
const PADDING: f32 = 4.0;
const GAP: f32 = 4.0;
const FONT_SIZE: u16 = 10;

const DRAG_HANDLE_W: f32 = 18.0;
const TOGGLE_W: f32 = 30.0;
const CHEVRON_W: f32 = 18.0;
const BADGE_W: f32 = 36.0;
const BADGE_H: f32 = 14.0;
const BADGE_RADIUS: f32 = 7.0;

const BORDER_W: f32 = 1.0;
const CORNER_RADIUS: f32 = 4.0;
const CARD_BOTTOM_MARGIN: f32 = 4.0;

const DE_BUTTON_SIZE: f32 = 20.0;
const DE_BUTTON_GAP: f32 = 2.0;

const DRIVER_CONFIG_HEIGHT: f32 = 52.0;
const DRIVER_ROW_HEIGHT: f32 = 22.0;
const BEAT_DIV_BTN_W: f32 = 27.0;
const BEAT_DIV_SPACING: f32 = 1.0;
const WAVE_BTN_W: f32 = 30.0;
const DRIVER_PAD_H: f32 = 5.0;
const BEAT_DIV_COUNT: usize = 11;
const WAVEFORM_COUNT: usize = 5;

const ENV_CONFIG_HEIGHT: f32 = 55.0;
const ENV_ROW_HEIGHT: f32 = 22.0;
const ENV_LABEL_W: f32 = 17.0;
const ENV_PAD_H: f32 = 5.0;

const TRIM_BAR_W: f32 = 4.0;
const TARGET_BAR_W: f32 = 6.0;
const OVERLAY_INSET: f32 = 1.0;

const ENV_ADR_MAX: f32 = 8.0;
const ENV_S_MAX: f32 = 1.0;

// ── Beat division labels ─────────────────────────────────────────

const BEAT_DIV_LABELS: [&str; BEAT_DIV_COUNT] = [
    "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32", "64",
];

const WAVEFORM_LABELS: [&str; WAVEFORM_COUNT] = ["Sin", "Tri", "Saw", "Sqr", "Rnd"];

// ── Data types ───────────────────────────────────────────────────

/// Per-parameter configuration info provided by the app layer.
#[derive(Debug, Clone)]
pub struct EffectParamInfo {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
}

/// Configuration for creating an effect card.
#[derive(Debug, Clone)]
pub struct EffectCardConfig {
    pub effect_index: usize,
    pub name: String,
    pub enabled: bool,
    pub supports_envelopes: bool,
    pub params: Vec<EffectParamInfo>,
}

/// Per-parameter expansion and modulation state.
pub struct EffectCardState {
    pub driver_expanded: Vec<bool>,
    pub envelope_expanded: Vec<bool>,
    pub trim_min: Vec<f32>,
    pub trim_max: Vec<f32>,
    pub target_norm: Vec<f32>,
    pub env_attack: Vec<f32>,
    pub env_decay: Vec<f32>,
    pub env_sustain: Vec<f32>,
    pub env_release: Vec<f32>,
}

impl EffectCardState {
    pub fn new(param_count: usize) -> Self {
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
        }
    }
}

// ── Internal node ID structs ─────────────────────────────────────

struct DriverConfigIds {
    container_id: i32,
    beat_div_btn_ids: [i32; BEAT_DIV_COUNT],
    dot_btn_id: i32,
    triplet_btn_id: i32,
    wave_btn_ids: [i32; WAVEFORM_COUNT],
    reverse_btn_id: i32,
}

struct EnvelopeConfigIds {
    container_id: i32,
    attack_slider: SliderNodeIds,
    decay_slider: SliderNodeIds,
    sustain_slider: SliderNodeIds,
    release_slider: SliderNodeIds,
}

struct TrimHandleIds {
    fill_id: i32,
    min_bar_id: i32,
    max_bar_id: i32,
}

struct EnvelopeTargetIds {
    target_bar_id: i32,
}

// ── EffectCardPanel ──────────────────────────────────────────────

pub struct EffectCardPanel {
    // Identity
    effect_index: usize,

    // Configuration
    effect_name: String,
    enabled: bool,
    is_collapsed: bool,
    is_selected: bool,
    supports_envelopes: bool,
    param_info: Vec<EffectParamInfo>,

    // State
    state: EffectCardState,

    // Node IDs — card shell
    border_id: i32,
    inner_bg_id: i32,

    // Node IDs — header
    header_bg_id: i32,
    drag_icon_id: i32,
    name_label_id: i32,
    toggle_btn_id: i32,
    chevron_btn_id: i32,
    env_badge_bg_id: i32,
    drv_badge_bg_id: i32,

    // Node IDs — per-param
    slider_ids: Vec<Option<SliderNodeIds>>,
    driver_btn_ids: Vec<i32>,
    envelope_btn_ids: Vec<i32>,
    driver_config_ids: Vec<Option<DriverConfigIds>>,
    envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    trim_ids: Vec<Option<TrimHandleIds>>,
    target_ids: Vec<Option<EnvelopeTargetIds>>,

    // Drag state
    dragging_param: i32,
    dragging_env_param: i32,
    dragging_env_slot: usize,
    dragging_trim_param: i32,
    dragging_trim_is_min: bool,
    dragging_target_param: i32,

    // Param value cache (NaN = needs sync)
    param_cache: Vec<f32>,

    // Node range
    first_node: usize,
    node_count: usize,
}

impl EffectCardPanel {
    pub fn new() -> Self {
        Self {
            effect_index: 0,
            effect_name: String::new(),
            enabled: true,
            is_collapsed: false,
            is_selected: false,
            supports_envelopes: true,
            param_info: Vec::new(),
            state: EffectCardState::new(0),
            border_id: -1,
            inner_bg_id: -1,
            header_bg_id: -1,
            drag_icon_id: -1,
            name_label_id: -1,
            toggle_btn_id: -1,
            chevron_btn_id: -1,
            env_badge_bg_id: -1,
            drv_badge_bg_id: -1,
            slider_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            target_ids: Vec::new(),
            dragging_param: -1,
            dragging_env_param: -1,
            dragging_env_slot: 0,
            dragging_trim_param: -1,
            dragging_trim_is_min: false,
            dragging_target_param: -1,
            param_cache: Vec::new(),
            first_node: 0,
            node_count: 0,
        }
    }

    /// Configure with effect metadata. Call before build.
    pub fn configure(&mut self, config: &EffectCardConfig) {
        self.effect_index = config.effect_index;
        self.effect_name = config.name.clone();
        self.enabled = config.enabled;
        self.supports_envelopes = config.supports_envelopes;
        self.param_info = config.params.clone();

        let n = config.params.len();
        self.state = EffectCardState::new(n);
        self.slider_ids = vec![None; n];
        self.driver_btn_ids = vec![-1; n];
        self.envelope_btn_ids = vec![-1; n];
        self.driver_config_ids = Vec::new();
        self.driver_config_ids.resize_with(n, || None);
        self.envelope_config_ids = Vec::new();
        self.envelope_config_ids.resize_with(n, || None);
        self.trim_ids = Vec::new();
        self.trim_ids.resize_with(n, || None);
        self.target_ids = Vec::new();
        self.target_ids.resize_with(n, || None);
        self.param_cache = vec![f32::NAN; n];
    }

    pub fn effect_index(&self) -> usize { self.effect_index }
    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }
    pub fn is_dragging(&self) -> bool {
        self.dragging_param >= 0 || self.dragging_env_param >= 0
            || self.dragging_trim_param >= 0 || self.dragging_target_param >= 0
    }

    pub fn set_selected(&mut self, selected: bool) { self.is_selected = selected; }
    pub fn set_collapsed(&mut self, collapsed: bool) { self.is_collapsed = collapsed; }
    pub fn set_enabled(&mut self, enabled: bool) { self.enabled = enabled; }
    pub fn state_mut(&mut self) -> &mut EffectCardState { &mut self.state }

    pub fn compute_height(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed && !self.param_info.is_empty() {
            for i in 0..self.param_info.len() {
                h += ROW_HEIGHT + ROW_SPACING;
                if self.state.driver_expanded.get(i).copied().unwrap_or(false) {
                    h += DRIVER_CONFIG_HEIGHT;
                }
                if self.state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    h += ENV_CONFIG_HEIGHT;
                }
            }
        }
        h + CARD_BOTTOM_MARGIN
    }

    pub fn drag_handle_id(&self) -> i32 { self.drag_icon_id }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);

        let effect_name = self.effect_name.clone();

        // Border
        let border_color = if self.is_selected { color::SELECTED_BORDER } else { color::CARD_BORDER_C32 };
        self.border_id = tree.add_panel(
            -1, rect.x, rect.y, rect.width, self.compute_height() - CARD_BOTTOM_MARGIN,
            UIStyle {
                bg_color: border_color,
                corner_radius: CORNER_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;

        // Inner background
        let inner = Rect::new(
            rect.x + BORDER_W, rect.y + BORDER_W,
            rect.width - BORDER_W * 2.0,
            self.compute_height() - CARD_BOTTOM_MARGIN - BORDER_W * 2.0,
        );
        self.inner_bg_id = tree.add_panel(
            self.border_id, inner.x, inner.y, inner.width, inner.height,
            UIStyle {
                bg_color: color::EFFECT_CARD_INNER_BG_C32,
                corner_radius: CORNER_RADIUS - BORDER_W,
                ..UIStyle::default()
            },
        ) as i32;

        let inner_w = inner.width;
        let parent = self.inner_bg_id;

        // Header
        self.build_header(tree, parent, inner.x, inner.y, inner_w, &effect_name);

        // Param sliders
        if !self.is_collapsed && !self.param_info.is_empty() {
            self.build_sliders(tree, parent, inner.x, inner.y + HEADER_HEIGHT, inner_w);
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_header(&mut self, tree: &mut UITree, parent: i32, x: f32, y: f32, w: f32, name: &str) {
        // Header background
        self.header_bg_id = tree.add_panel(
            parent, x, y, w, HEADER_HEIGHT,
            UIStyle {
                bg_color: color::DRAG_HANDLE_BG_C32,
                corner_radius: CORNER_RADIUS - BORDER_W,
                ..UIStyle::default()
            },
        ) as i32;

        // Layout (right-to-left for fixed elements)
        let chevron_x = x + w - PADDING - CHEVRON_W;
        let toggle_x = chevron_x - GAP - TOGGLE_W;
        let drv_x = toggle_x - GAP - BADGE_W;
        let env_x = drv_x - GAP - BADGE_W;
        let name_x = x + PADDING + DRAG_HANDLE_W + GAP;
        let name_w = (env_x - GAP - name_x).max(10.0);
        let elem_y = y + (HEADER_HEIGHT - 16.0) * 0.5;
        let badge_y = y + (HEADER_HEIGHT - BADGE_H) * 0.5;

        // Drag handle
        self.drag_icon_id = tree.add_button(
            self.header_bg_id, x + PADDING, elem_y, DRAG_HANDLE_W, 16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::DRAG_HANDLE_HOVER_BG_C32,
                pressed_bg_color: color::DRAG_HANDLE_BG_C32,
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "\u{2261}", // ≡
        ) as i32;

        // Name label
        self.name_label_id = tree.add_label(
            self.header_bg_id, name_x, elem_y, name_w, 16.0,
            name,
            UIStyle {
                text_color: color::EFFECT_HEADER_NAME,
                font_size: FONT_SIZE,
                font_weight: FontWeight::Bold,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        // ENV badge (hidden initially)
        self.env_badge_bg_id = tree.add_panel(
            self.header_bg_id, env_x, badge_y, BADGE_W, BADGE_H,
            UIStyle {
                bg_color: color::ENVELOPE_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.env_badge_bg_id as u32, false);

        // DRV badge (hidden initially)
        self.drv_badge_bg_id = tree.add_panel(
            self.header_bg_id, drv_x, badge_y, BADGE_W, BADGE_H,
            UIStyle {
                bg_color: color::DRIVER_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.drv_badge_bg_id as u32, false);

        // Toggle button (ON/OFF)
        let toggle_style = toggle_btn_style(self.enabled);
        self.toggle_btn_id = tree.add_button(
            self.header_bg_id, toggle_x, elem_y, TOGGLE_W, 16.0,
            toggle_style,
            if self.enabled { "ON" } else { "OFF" },
        ) as i32;

        // Chevron
        self.chevron_btn_id = tree.add_button(
            self.header_bg_id, chevron_x, elem_y, CHEVRON_W, 16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.is_collapsed { "\u{25B6}" } else { "\u{25BC}" },
        ) as i32;
    }

    fn build_sliders(&mut self, tree: &mut UITree, parent: i32, x: f32, start_y: f32, w: f32) {
        let mut cy = start_y;
        let slider_w = w - PADDING * 2.0 - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 2.0;

        for i in 0..self.param_info.len() {
            let info = self.param_info[i].clone();
            let norm = BitmapSlider::value_to_normalized(info.default, info.min, info.max);
            let val_text = format_param_value(info.default, info.whole_numbers);

            // Param slider
            let slider_rect = Rect::new(x + PADDING, cy, slider_w, ROW_HEIGHT);
            self.slider_ids[i] = Some(BitmapSlider::build(
                tree, parent, slider_rect,
                Some(&info.name), norm,
                &val_text, &SliderColors::default_slider(),
                FONT_SIZE, crate::slider::DEFAULT_LABEL_WIDTH,
            ));

            // Trim handles (if driver expanded)
            if self.state.driver_expanded.get(i).copied().unwrap_or(false) {
                if let Some(ref slider) = self.slider_ids[i] {
                    self.trim_ids[i] = Some(self.build_trim_handles(
                        tree, slider.track as i32, slider.track_rect, i,
                    ));
                }
            }

            // Envelope target (if envelope expanded)
            if self.state.envelope_expanded.get(i).copied().unwrap_or(false) {
                if let Some(ref slider) = self.slider_ids[i] {
                    self.target_ids[i] = Some(self.build_envelope_target(
                        tree, slider.track as i32, slider.track_rect, i,
                    ));
                }
            }

            // D/E buttons (right side of row)
            let btn_x = x + PADDING + slider_w + DE_BUTTON_GAP;
            let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;

            if self.supports_envelopes {
                let env_active = self.state.envelope_expanded.get(i).copied().unwrap_or(false);
                self.envelope_btn_ids[i] = tree.add_button(
                    parent, btn_x, btn_y, DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                    de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
                    "E",
                ) as i32;
            }

            let drv_active = self.state.driver_expanded.get(i).copied().unwrap_or(false);
            let drv_btn_x = btn_x + if self.supports_envelopes { DE_BUTTON_SIZE + DE_BUTTON_GAP } else { 0.0 };
            self.driver_btn_ids[i] = tree.add_button(
                parent, drv_btn_x, btn_y, DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
                "\u{2192}", // →
            ) as i32;

            cy += ROW_HEIGHT + ROW_SPACING;

            // Envelope config drawer
            if self.state.envelope_expanded.get(i).copied().unwrap_or(false) {
                self.envelope_config_ids[i] = Some(self.build_envelope_config(tree, parent, x + PADDING, cy, slider_w, i));
                cy += ENV_CONFIG_HEIGHT;
            }

            // Driver config drawer
            if self.state.driver_expanded.get(i).copied().unwrap_or(false) {
                self.driver_config_ids[i] = Some(self.build_driver_config(tree, parent, x + PADDING, cy, slider_w, i));
                cy += DRIVER_CONFIG_HEIGHT;
            }
        }
    }

    fn build_trim_handles(&self, tree: &mut UITree, track_parent: i32, track_rect: Rect, param_idx: usize) -> TrimHandleIds {
        let usable = track_rect.width - OVERLAY_INSET * 2.0;
        let tmin = self.state.trim_min.get(param_idx).copied().unwrap_or(0.0);
        let tmax = self.state.trim_max.get(param_idx).copied().unwrap_or(1.0);

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

    fn build_envelope_target(&self, tree: &mut UITree, track_parent: i32, track_rect: Rect, param_idx: usize) -> EnvelopeTargetIds {
        let usable = track_rect.width - OVERLAY_INSET * 2.0;
        let norm = self.state.target_norm.get(param_idx).copied().unwrap_or(0.5);
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

    #[allow(clippy::too_many_arguments)]
    fn build_driver_config(&self, tree: &mut UITree, parent: i32, x: f32, y: f32, w: f32, _param_idx: usize) -> DriverConfigIds {
        let container_id = tree.add_panel(
            parent, x, y, w, DRIVER_CONFIG_HEIGHT,
            UIStyle { bg_color: color::CONFIG_BG_C32, corner_radius: 2.0, ..UIStyle::default() },
        ) as i32;

        let mut cx = x + DRIVER_PAD_H;
        let row1_y = y + 4.0;

        // Beat division buttons (row 1)
        let mut beat_div_btn_ids = [-1i32; BEAT_DIV_COUNT];
        let avail_w = w - DRIVER_PAD_H * 2.0;
        let btn_w = (avail_w - BEAT_DIV_SPACING * (BEAT_DIV_COUNT as f32 - 1.0)) / BEAT_DIV_COUNT as f32;

        for j in 0..BEAT_DIV_COUNT {
            beat_div_btn_ids[j] = tree.add_button(
                container_id, cx, row1_y, btn_w, DRIVER_ROW_HEIGHT,
                UIStyle {
                    bg_color: color::CONFIG_BTN_INACTIVE_C32,
                    hover_bg_color: color::CONFIG_BTN_HOVER_C32,
                    pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: 8,
                    corner_radius: 1.0,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
                BEAT_DIV_LABELS[j],
            ) as i32;
            cx += btn_w + BEAT_DIV_SPACING;
        }

        // Row 2: dot, triplet, waveforms, reverse
        let row2_y = row1_y + DRIVER_ROW_HEIGHT + 4.0;
        cx = x + DRIVER_PAD_H;

        let dot_btn_id = tree.add_button(
            container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
            UIStyle {
                bg_color: color::CONFIG_BTN_INACTIVE_C32,
                hover_bg_color: color::CONFIG_BTN_HOVER_C32,
                pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                corner_radius: 1.0,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            ".",
        ) as i32;
        cx += WAVE_BTN_W + BEAT_DIV_SPACING;

        let triplet_btn_id = tree.add_button(
            container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
            UIStyle {
                bg_color: color::CONFIG_BTN_INACTIVE_C32,
                hover_bg_color: color::CONFIG_BTN_HOVER_C32,
                pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                corner_radius: 1.0,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "T",
        ) as i32;
        cx += WAVE_BTN_W + GAP;

        // Waveform buttons
        let mut wave_btn_ids = [-1i32; WAVEFORM_COUNT];
        for j in 0..WAVEFORM_COUNT {
            wave_btn_ids[j] = tree.add_button(
                container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
                UIStyle {
                    bg_color: color::CONFIG_BTN_INACTIVE_C32,
                    hover_bg_color: color::CONFIG_BTN_HOVER_C32,
                    pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: 8,
                    corner_radius: 1.0,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
                WAVEFORM_LABELS[j],
            ) as i32;
            cx += WAVE_BTN_W + BEAT_DIV_SPACING;
        }

        // Reverse button
        let reverse_w = 32.0;
        let reverse_x = x + w - DRIVER_PAD_H - reverse_w;
        let reverse_btn_id = tree.add_button(
            container_id, reverse_x, row2_y, reverse_w, DRIVER_ROW_HEIGHT,
            UIStyle {
                bg_color: color::CONFIG_BTN_INACTIVE_C32,
                hover_bg_color: color::CONFIG_BTN_HOVER_C32,
                pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
                text_color: color::TEXT_DIMMED_C32,
                font_size: 8,
                corner_radius: 1.0,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "Rev",
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

    fn build_envelope_config(&self, tree: &mut UITree, parent: i32, x: f32, y: f32, w: f32, param_idx: usize) -> EnvelopeConfigIds {
        let container_id = tree.add_panel(
            parent, x, y, w, ENV_CONFIG_HEIGHT,
            UIStyle { bg_color: color::CONFIG_BG_C32, corner_radius: 2.0, ..UIStyle::default() },
        ) as i32;

        let half_w = (w - ENV_PAD_H * 2.0 - GAP) * 0.5;
        let sx = x + ENV_PAD_H;
        let row1_y = y + 4.0;
        let row2_y = row1_y + ENV_ROW_HEIGHT + 4.0;

        let attack_val = self.state.env_attack.get(param_idx).copied().unwrap_or(0.1);
        let decay_val = self.state.env_decay.get(param_idx).copied().unwrap_or(0.3);
        let sustain_val = self.state.env_sustain.get(param_idx).copied().unwrap_or(0.7);
        let release_val = self.state.env_release.get(param_idx).copied().unwrap_or(0.5);

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

    // ── Sync methods ─────────────────────────────────────────────

    /// Push param values from the engine. Updates sliders on change.
    pub fn sync_values(&mut self, tree: &mut UITree, values: &[f32]) {
        for (i, &val) in values.iter().enumerate().take(self.param_info.len()) {
            if val != self.param_cache[i] || self.param_cache[i].is_nan() {
                self.param_cache[i] = val;
                if let Some(ref ids) = self.slider_ids[i] {
                    let info = &self.param_info[i];
                    let norm = BitmapSlider::value_to_normalized(val, info.min, info.max);
                    let text = format_param_value(val, info.whole_numbers);
                    BitmapSlider::update_value(tree, ids, norm, &text);
                }
            }
        }
    }

    pub fn sync_effect_name(&mut self, tree: &mut UITree, name: &str) {
        self.effect_name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    pub fn sync_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        self.enabled = enabled;
        if self.toggle_btn_id >= 0 {
            tree.set_style(self.toggle_btn_id as u32, toggle_btn_style(enabled));
            tree.set_text(self.toggle_btn_id as u32, if enabled { "ON" } else { "OFF" });
        }
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&mut self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        let ei = self.effect_index;

        // Header buttons
        if id == self.toggle_btn_id {
            return vec![PanelAction::EffectToggle(ei)];
        }
        if id == self.chevron_btn_id {
            return vec![PanelAction::EffectCollapseToggle(ei)];
        }

        // D/E buttons
        for (pi, &btn_id) in self.driver_btn_ids.iter().enumerate() {
            if id == btn_id {
                return vec![PanelAction::EffectDriverToggle(ei, pi)];
            }
        }
        for (pi, &btn_id) in self.envelope_btn_ids.iter().enumerate() {
            if id == btn_id {
                return vec![PanelAction::EffectEnvelopeToggle(ei, pi)];
            }
        }

        // Driver config buttons
        for (pi, cfg) in self.driver_config_ids.iter().enumerate() {
            if let Some(ref c) = cfg {
                for (j, &bid) in c.beat_div_btn_ids.iter().enumerate() {
                    if id == bid { return vec![PanelAction::EffectDriverConfig(ei, pi, DriverConfigAction::BeatDiv(j))]; }
                }
                if id == c.dot_btn_id { return vec![PanelAction::EffectDriverConfig(ei, pi, DriverConfigAction::Dot)]; }
                if id == c.triplet_btn_id { return vec![PanelAction::EffectDriverConfig(ei, pi, DriverConfigAction::Triplet)]; }
                for (j, &wid) in c.wave_btn_ids.iter().enumerate() {
                    if id == wid { return vec![PanelAction::EffectDriverConfig(ei, pi, DriverConfigAction::Wave(j))]; }
                }
                if id == c.reverse_btn_id { return vec![PanelAction::EffectDriverConfig(ei, pi, DriverConfigAction::Reverse)]; }
            }
        }

        // Card selection (any click on header bg)
        if id == self.header_bg_id || id == self.drag_icon_id || id == self.name_label_id {
            return vec![PanelAction::EffectCardClicked(ei)];
        }

        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        let ei = self.effect_index;

        // Check envelope target bars first (highest priority)
        for (pi, target) in self.target_ids.iter().enumerate() {
            if let Some(ref t) = target {
                if node_id as i32 == t.target_bar_id {
                    self.dragging_target_param = pi as i32;
                    return Vec::new();
                }
            }
        }

        // Check trim bars
        for (pi, trim) in self.trim_ids.iter().enumerate() {
            if let Some(ref t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.dragging_trim_param = pi as i32;
                    self.dragging_trim_is_min = true;
                    return Vec::new();
                }
                if node_id as i32 == t.max_bar_id {
                    self.dragging_trim_param = pi as i32;
                    self.dragging_trim_is_min = false;
                    return Vec::new();
                }
            }
        }

        // Check ADSR slider tracks
        for (pi, env_cfg) in self.envelope_config_ids.iter().enumerate() {
            if let Some(ref c) = env_cfg {
                if node_id == c.attack_slider.track {
                    self.dragging_env_param = pi as i32;
                    self.dragging_env_slot = 0;
                    let norm = BitmapSlider::x_to_normalized(c.attack_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Attack, norm * ENV_ADR_MAX)];
                }
                if node_id == c.decay_slider.track {
                    self.dragging_env_param = pi as i32;
                    self.dragging_env_slot = 1;
                    let norm = BitmapSlider::x_to_normalized(c.decay_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Decay, norm * ENV_ADR_MAX)];
                }
                if node_id == c.sustain_slider.track {
                    self.dragging_env_param = pi as i32;
                    self.dragging_env_slot = 2;
                    let norm = BitmapSlider::x_to_normalized(c.sustain_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Sustain, norm * ENV_S_MAX)];
                }
                if node_id == c.release_slider.track {
                    self.dragging_env_param = pi as i32;
                    self.dragging_env_slot = 3;
                    let norm = BitmapSlider::x_to_normalized(c.release_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Release, norm * ENV_ADR_MAX)];
                }
            }
        }

        // Check param slider tracks
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let Some(ref ids) = slider {
                if node_id == ids.track {
                    self.dragging_param = pi as i32;
                    let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                    let info = &self.param_info[pi];
                    let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                    let val = if info.whole_numbers { val.round() } else { val };
                    return vec![
                        PanelAction::EffectParamSnapshot(ei, pi),
                        PanelAction::EffectParamChanged(ei, pi, val),
                    ];
                }
            }
        }

        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        // Target bar drag
        if self.dragging_target_param >= 0 {
            let pi = self.dragging_target_param as usize;
            if let Some(ref slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                if let Some(ref mut state) = self.state.target_norm.get_mut(pi) {
                    **state = norm;
                }
                return vec![PanelAction::EffectTargetChanged(ei, pi, norm)];
            }
        }

        // Trim bar drag
        if self.dragging_trim_param >= 0 {
            let pi = self.dragging_trim_param as usize;
            if let Some(ref slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                let tmin = self.state.trim_min.get(pi).copied().unwrap_or(0.0);
                let tmax = self.state.trim_max.get(pi).copied().unwrap_or(1.0);
                let (new_min, new_max) = if self.dragging_trim_is_min {
                    (norm.min(tmax), tmax)
                } else {
                    (tmin, norm.max(tmin))
                };
                if let Some(v) = self.state.trim_min.get_mut(pi) { *v = new_min; }
                if let Some(v) = self.state.trim_max.get_mut(pi) { *v = new_max; }
                return vec![PanelAction::EffectTrimChanged(ei, pi, new_min, new_max)];
            }
        }

        // ADSR drag
        if self.dragging_env_param >= 0 {
            let pi = self.dragging_env_param as usize;
            if let Some(ref cfg) = self.envelope_config_ids.get(pi).and_then(|c| c.as_ref()) {
                let (slider, param, max) = match self.dragging_env_slot {
                    0 => (&cfg.attack_slider, EnvelopeParam::Attack, ENV_ADR_MAX),
                    1 => (&cfg.decay_slider, EnvelopeParam::Decay, ENV_ADR_MAX),
                    2 => (&cfg.sustain_slider, EnvelopeParam::Sustain, ENV_S_MAX),
                    _ => (&cfg.release_slider, EnvelopeParam::Release, ENV_ADR_MAX),
                };
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                let val = norm * max;
                let text = format!("{:.2}", val);
                BitmapSlider::update_value(tree, slider, norm, &text);
                return vec![PanelAction::EffectEnvParamChanged(ei, pi, param, val)];
            }
        }

        // Param slider drag
        if self.dragging_param >= 0 {
            let pi = self.dragging_param as usize;
            if let Some(ref ids) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let info = &self.param_info[pi];
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                let val = if info.whole_numbers { val.round() } else { val };
                let text = format_param_value(val, info.whole_numbers);
                BitmapSlider::update_value(tree, ids, norm, &text);
                return vec![PanelAction::EffectParamChanged(ei, pi, val)];
            }
        }

        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        if self.dragging_target_param >= 0 {
            self.dragging_target_param = -1;
            return Vec::new(); // target changes are auto-committed
        }
        if self.dragging_trim_param >= 0 {
            self.dragging_trim_param = -1;
            return Vec::new(); // trim changes are auto-committed
        }
        if self.dragging_env_param >= 0 {
            self.dragging_env_param = -1;
            return Vec::new(); // ADSR changes are auto-committed
        }
        if self.dragging_param >= 0 {
            let pi = self.dragging_param as usize;
            self.dragging_param = -1;
            return vec![PanelAction::EffectParamCommit(ei, pi)];
        }

        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        let ei = self.effect_index;
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let Some(ref ids) = slider {
                if node_id == ids.track {
                    let default = self.param_info.get(pi).map(|i| i.default).unwrap_or(0.0);
                    return vec![PanelAction::EffectParamRightClick(ei, pi, default)];
                }
            }
        }
        Vec::new()
    }
}

impl Default for EffectCardPanel {
    fn default() -> Self { Self::new() }
}

// ── Helpers ──────────────────────────────────────────────────────

fn format_param_value(val: f32, whole_numbers: bool) -> String {
    if whole_numbers { format!("{}", val as i32) } else { format!("{:.2}", val) }
}

fn toggle_btn_style(enabled: bool) -> UIStyle {
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

fn de_btn_style(active: bool, active_color: Color32) -> UIStyle {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    fn test_config() -> EffectCardConfig {
        EffectCardConfig {
            effect_index: 0,
            name: "Blur".into(),
            enabled: true,
            supports_envelopes: true,
            params: vec![
                EffectParamInfo { name: "Radius".into(), min: 0.0, max: 100.0, default: 10.0, whole_numbers: true },
                EffectParamInfo { name: "Strength".into(), min: 0.0, max: 1.0, default: 0.5, whole_numbers: false },
            ],
        }
    }

    #[test]
    fn build_effect_card() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        assert!(panel.border_id >= 0);
        assert!(panel.inner_bg_id >= 0);
        assert!(panel.header_bg_id >= 0);
        assert!(panel.drag_icon_id >= 0);
        assert!(panel.name_label_id >= 0);
        assert!(panel.toggle_btn_id >= 0);
        assert!(panel.chevron_btn_id >= 0);
        assert_eq!(panel.slider_ids.len(), 2);
        assert!(panel.slider_ids[0].is_some());
        assert!(panel.slider_ids[1].is_some());
        assert!(panel.node_count > 0);
    }

    #[test]
    fn handle_click_toggle() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.toggle_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectToggle(0)));
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.chevron_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectCollapseToggle(0)));
    }

    #[test]
    fn handle_click_driver_button() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.driver_btn_ids[0] as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectDriverToggle(0, 0)));
    }

    #[test]
    fn sync_values_updates_slider() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        tree.clear_dirty();
        panel.sync_values(&mut tree, &[50.0, 0.8]);
        assert!(tree.has_dirty());
    }

    #[test]
    fn compute_height_collapsed() {
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());

        let expanded_h = panel.compute_height();
        panel.set_collapsed(true);
        let collapsed_h = panel.compute_height();

        assert!(collapsed_h < expanded_h);
    }

    #[test]
    fn effect_card_with_driver_expanded() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.state.driver_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.driver_config_ids[0].is_some());
        assert!(panel.trim_ids[0].is_some());
    }

    #[test]
    fn effect_card_with_envelope_expanded() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.envelope_config_ids[0].is_some());
        assert!(panel.target_ids[0].is_some());
    }
}
