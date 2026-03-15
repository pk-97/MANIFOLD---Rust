use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::{PanelAction, DriverConfigAction, EnvelopeParam};

// ── Layout constants (from GenParamBitmapPanel.cs) ────────────────

const ROW_HEIGHT: f32 = 20.0;
const ROW_SPACING: f32 = 2.0;
const PADDING: f32 = 4.0;
const GAP: f32 = 4.0;
const FONT_SIZE: u16 = 10;

const GEN_TYPE_ROW_H: f32 = 22.0;
const SECTION_LABEL_H: f32 = 18.0;
const DIVIDER_H: f32 = 1.0;
const TOGGLE_BTN_W: f32 = 40.0;
const TOGGLE_BTN_H: f32 = 16.0;

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

const CHANGE_BTN_W: f32 = 100.0;

const BEAT_DIV_LABELS: [&str; BEAT_DIV_COUNT] = [
    "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32", "64",
];
const WAVEFORM_LABELS: [&str; WAVEFORM_COUNT] = ["Sin", "Tri", "Saw", "Sqr", "Rnd"];

// ── Panel-specific colors ────────────────────────────────────────

const GEN_TYPE_HOVER: Color32 = Color32::new(40, 40, 44, 255);
const GEN_TYPE_LABEL_COLOR: Color32 = Color32::new(150, 200, 150, 255);

// ── Data types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GenParamInfo {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    pub is_toggle: bool,
}

#[derive(Debug, Clone)]
pub struct GenParamConfig {
    pub gen_type_name: String,
    pub params: Vec<GenParamInfo>,
}

/// Per-parameter expansion state for generator params.
pub struct GenParamState {
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

impl GenParamState {
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

struct ToggleParamIds {
    label_id: i32,
    button_id: i32,
}

struct DriverConfigIds {
    beat_div_btn_ids: [i32; BEAT_DIV_COUNT],
    dot_btn_id: i32,
    triplet_btn_id: i32,
    wave_btn_ids: [i32; WAVEFORM_COUNT],
    reverse_btn_id: i32,
}

struct EnvelopeConfigIds {
    attack_slider: SliderNodeIds,
    decay_slider: SliderNodeIds,
    sustain_slider: SliderNodeIds,
    release_slider: SliderNodeIds,
}

struct TrimHandleIds {
    min_bar_id: i32,
    max_bar_id: i32,
}

struct EnvelopeTargetIds {
    target_bar_id: i32,
}

// ── GenParamPanel ────────────────────────────────────────────────

pub struct GenParamPanel {
    // Configuration
    gen_type_name: String,
    param_info: Vec<GenParamInfo>,
    state: GenParamState,

    // Node IDs — gen type row
    gen_type_label_id: i32,
    gen_type_btn_id: i32,

    // Node IDs — per-param (sliders or toggles)
    slider_ids: Vec<Option<SliderNodeIds>>,
    toggle_ids: Vec<Option<ToggleParamIds>>,
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

    // Cache
    param_cache: Vec<f32>,
    toggle_cache: Vec<bool>,

    // Node range
    first_node: usize,
    node_count: usize,
}

impl GenParamPanel {
    pub fn new() -> Self {
        Self {
            gen_type_name: String::new(),
            param_info: Vec::new(),
            state: GenParamState::new(0),
            gen_type_label_id: -1,
            gen_type_btn_id: -1,
            slider_ids: Vec::new(),
            toggle_ids: Vec::new(),
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
            toggle_cache: Vec::new(),
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn configure(&mut self, config: &GenParamConfig) {
        self.gen_type_name = config.gen_type_name.clone();
        self.param_info = config.params.clone();

        let n = config.params.len();
        self.state = GenParamState::new(n);
        self.slider_ids = vec![None; n];
        self.toggle_ids = Vec::new();
        self.toggle_ids.resize_with(n, || None);
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
        self.toggle_cache = vec![false; n];
    }

    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }
    pub fn state_mut(&mut self) -> &mut GenParamState { &mut self.state }
    pub fn is_dragging(&self) -> bool {
        self.dragging_param >= 0 || self.dragging_env_param >= 0
            || self.dragging_trim_param >= 0 || self.dragging_target_param >= 0
    }

    pub fn compute_height(&self) -> f32 {
        let mut h = GEN_TYPE_ROW_H + DIVIDER_H + SECTION_LABEL_H;
        for (i, info) in self.param_info.iter().enumerate() {
            if info.is_toggle {
                h += ROW_HEIGHT + ROW_SPACING;
            } else {
                h += ROW_HEIGHT + ROW_SPACING;
                if self.state.driver_expanded.get(i).copied().unwrap_or(false) {
                    h += DRIVER_CONFIG_HEIGHT;
                }
                if self.state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    h += ENV_CONFIG_HEIGHT;
                }
            }
        }
        h + PADDING
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.toggle_cache.iter_mut().for_each(|v| *v = false);

        let content_w = rect.width - PADDING * 2.0;
        let cx = rect.x + PADDING;
        let mut cy = rect.y;

        let gen_name = self.gen_type_name.clone();

        // Gen type row
        let label_w = content_w - CHANGE_BTN_W - GAP;
        self.gen_type_label_id = tree.add_label(
            -1, cx, cy, label_w, GEN_TYPE_ROW_H,
            &gen_name,
            UIStyle {
                text_color: GEN_TYPE_LABEL_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        self.gen_type_btn_id = tree.add_button(
            -1, cx + label_w + GAP, cy + (GEN_TYPE_ROW_H - 18.0) * 0.5,
            CHANGE_BTN_W, 18.0,
            UIStyle {
                bg_color: color::CONFIG_BG_C32,
                hover_bg_color: GEN_TYPE_HOVER,
                pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                corner_radius: 2.0,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "Change \u{25BC}",
        ) as i32;
        cy += GEN_TYPE_ROW_H;

        // Divider + section label
        tree.add_panel(
            -1, cx, cy, content_w, DIVIDER_H,
            UIStyle { bg_color: color::DIVIDER_C32, ..UIStyle::default() },
        );
        cy += DIVIDER_H;

        tree.add_label(
            -1, cx, cy, content_w, SECTION_LABEL_H,
            "Generator",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                font_weight: FontWeight::Bold,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += SECTION_LABEL_H;

        // Params
        let slider_w = content_w - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 2.0;

        for i in 0..self.param_info.len() {
            let info = self.param_info[i].clone();

            if info.is_toggle {
                // Toggle row
                let label_id = tree.add_label(
                    -1, cx, cy, content_w - TOGGLE_BTN_W - GAP, ROW_HEIGHT,
                    &info.name,
                    UIStyle {
                        text_color: color::SLIDER_TEXT_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                ) as i32;

                let on = info.default > 0.5;
                let button_id = tree.add_button(
                    -1, cx + content_w - TOGGLE_BTN_W,
                    cy + (ROW_HEIGHT - TOGGLE_BTN_H) * 0.5,
                    TOGGLE_BTN_W, TOGGLE_BTN_H,
                    toggle_style(on),
                    if on { "ON" } else { "OFF" },
                ) as i32;

                self.toggle_ids[i] = Some(ToggleParamIds { label_id, button_id });
                self.toggle_cache[i] = on;
                cy += ROW_HEIGHT + ROW_SPACING;
            } else {
                // Slider row
                let norm = BitmapSlider::value_to_normalized(info.default, info.min, info.max);
                let val_text = format_param_value(info.default, info.whole_numbers);
                let slider_rect = Rect::new(cx, cy, slider_w, ROW_HEIGHT);
                self.slider_ids[i] = Some(BitmapSlider::build(
                    tree, -1, slider_rect,
                    Some(&info.name), norm,
                    &val_text, &SliderColors::default_slider(),
                    FONT_SIZE, crate::slider::DEFAULT_LABEL_WIDTH,
                ));

                // Trim handles (if driver expanded)
                if self.state.driver_expanded.get(i).copied().unwrap_or(false) {
                    if let Some(ref slider) = self.slider_ids[i] {
                        let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                        let tmin = self.state.trim_min.get(i).copied().unwrap_or(0.0);
                        let tmax = self.state.trim_max.get(i).copied().unwrap_or(1.0);
                        let min_x = slider.track_rect.x + OVERLAY_INSET + tmin * usable - TRIM_BAR_W * 0.5;
                        let max_x = slider.track_rect.x + OVERLAY_INSET + tmax * usable - TRIM_BAR_W * 0.5;

                        let min_bar_id = tree.add_button(
                            slider.track as i32, min_x, slider.track_rect.y,
                            TRIM_BAR_W, slider.track_rect.height,
                            UIStyle {
                                bg_color: color::DRIVER_ACTIVE_C32,
                                hover_bg_color: color::TRIM_BAR_HOVER_C32,
                                corner_radius: 1.0,
                                ..UIStyle::default()
                            },
                            "",
                        ) as i32;
                        let max_bar_id = tree.add_button(
                            slider.track as i32, max_x, slider.track_rect.y,
                            TRIM_BAR_W, slider.track_rect.height,
                            UIStyle {
                                bg_color: color::DRIVER_ACTIVE_C32,
                                hover_bg_color: color::TRIM_BAR_HOVER_C32,
                                corner_radius: 1.0,
                                ..UIStyle::default()
                            },
                            "",
                        ) as i32;
                        self.trim_ids[i] = Some(TrimHandleIds { min_bar_id, max_bar_id });
                    }
                }

                // Envelope target
                if self.state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    if let Some(ref slider) = self.slider_ids[i] {
                        let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                        let norm_t = self.state.target_norm.get(i).copied().unwrap_or(0.5);
                        let bar_x = slider.track_rect.x + OVERLAY_INSET + norm_t * usable - TARGET_BAR_W * 0.5;
                        let target_bar_id = tree.add_button(
                            slider.track as i32, bar_x, slider.track_rect.y - 2.0,
                            TARGET_BAR_W, slider.track_rect.height + 4.0,
                            UIStyle {
                                bg_color: color::ENVELOPE_ACTIVE_C32,
                                hover_bg_color: color::TARGET_BAR_HOVER_C32,
                                corner_radius: 1.0,
                                ..UIStyle::default()
                            },
                            "",
                        ) as i32;
                        self.target_ids[i] = Some(EnvelopeTargetIds { target_bar_id });
                    }
                }

                // D/E buttons
                let btn_x = cx + slider_w + DE_BUTTON_GAP;
                let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;

                let env_active = self.state.envelope_expanded.get(i).copied().unwrap_or(false);
                self.envelope_btn_ids[i] = tree.add_button(
                    -1, btn_x, btn_y, DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                    de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
                    "E",
                ) as i32;

                let drv_active = self.state.driver_expanded.get(i).copied().unwrap_or(false);
                self.driver_btn_ids[i] = tree.add_button(
                    -1, btn_x + DE_BUTTON_SIZE + DE_BUTTON_GAP, btn_y,
                    DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                    de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
                    "\u{2192}",
                ) as i32;

                cy += ROW_HEIGHT + ROW_SPACING;

                // Envelope config
                if self.state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    self.envelope_config_ids[i] = Some(self.build_envelope_config(tree, cx, cy, slider_w, i));
                    cy += ENV_CONFIG_HEIGHT;
                }

                // Driver config
                if self.state.driver_expanded.get(i).copied().unwrap_or(false) {
                    self.driver_config_ids[i] = Some(self.build_driver_config(tree, cx, cy, slider_w));
                    cy += DRIVER_CONFIG_HEIGHT;
                }
            }
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_driver_config(&self, tree: &mut UITree, x: f32, y: f32, w: f32) -> DriverConfigIds {
        let container_id = tree.add_panel(
            -1, x, y, w, DRIVER_CONFIG_HEIGHT,
            UIStyle { bg_color: color::CONFIG_BG_C32, corner_radius: 2.0, ..UIStyle::default() },
        ) as i32;

        let mut cx = x + DRIVER_PAD_H;
        let row1_y = y + 4.0;
        let avail_w = w - DRIVER_PAD_H * 2.0;
        let btn_w = (avail_w - BEAT_DIV_SPACING * (BEAT_DIV_COUNT as f32 - 1.0)) / BEAT_DIV_COUNT as f32;

        let mut beat_div_btn_ids = [-1i32; BEAT_DIV_COUNT];
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

        let row2_y = row1_y + DRIVER_ROW_HEIGHT + 4.0;
        cx = x + DRIVER_PAD_H;

        let dot_btn_id = tree.add_button(
            container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
            config_btn_style(), ".",
        ) as i32;
        cx += WAVE_BTN_W + BEAT_DIV_SPACING;

        let triplet_btn_id = tree.add_button(
            container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
            config_btn_style(), "T",
        ) as i32;
        cx += WAVE_BTN_W + GAP;

        let mut wave_btn_ids = [-1i32; WAVEFORM_COUNT];
        for j in 0..WAVEFORM_COUNT {
            wave_btn_ids[j] = tree.add_button(
                container_id, cx, row2_y, WAVE_BTN_W, DRIVER_ROW_HEIGHT,
                config_btn_style(), WAVEFORM_LABELS[j],
            ) as i32;
            cx += WAVE_BTN_W + BEAT_DIV_SPACING;
        }

        let reverse_w = 32.0;
        let reverse_x = x + w - DRIVER_PAD_H - reverse_w;
        let reverse_btn_id = tree.add_button(
            container_id, reverse_x, row2_y, reverse_w, DRIVER_ROW_HEIGHT,
            config_btn_style(), "Rev",
        ) as i32;

        DriverConfigIds {
            beat_div_btn_ids,
            dot_btn_id,
            triplet_btn_id,
            wave_btn_ids,
            reverse_btn_id,
        }
    }

    fn build_envelope_config(&self, tree: &mut UITree, x: f32, y: f32, w: f32, param_idx: usize) -> EnvelopeConfigIds {
        let container_id = tree.add_panel(
            -1, x, y, w, ENV_CONFIG_HEIGHT,
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

        EnvelopeConfigIds { attack_slider, decay_slider, sustain_slider, release_slider }
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_values(&mut self, tree: &mut UITree, values: &[f32]) {
        for (i, &val) in values.iter().enumerate().take(self.param_info.len()) {
            let info = &self.param_info[i];
            if info.is_toggle {
                let on = val > 0.5;
                if on != self.toggle_cache[i] {
                    self.toggle_cache[i] = on;
                    if let Some(ref ids) = self.toggle_ids[i] {
                        tree.set_style(ids.button_id as u32, toggle_style(on));
                        tree.set_text(ids.button_id as u32, if on { "ON" } else { "OFF" });
                    }
                }
            } else if val != self.param_cache[i] || self.param_cache[i].is_nan() {
                self.param_cache[i] = val;
                if let Some(ref ids) = self.slider_ids[i] {
                    let norm = BitmapSlider::value_to_normalized(val, info.min, info.max);
                    let text = format_param_value(val, info.whole_numbers);
                    BitmapSlider::update_value(tree, ids, norm, &text);
                }
            }
        }
    }

    pub fn sync_gen_type_name(&mut self, tree: &mut UITree, name: &str) {
        self.gen_type_name = name.into();
        if self.gen_type_label_id >= 0 {
            tree.set_text(self.gen_type_label_id as u32, name);
        }
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&mut self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;

        if id == self.gen_type_btn_id {
            return vec![PanelAction::GenTypeClicked];
        }

        // Toggle buttons
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(ref t) = toggle {
                if id == t.button_id {
                    return vec![PanelAction::GenParamToggle(pi)];
                }
            }
        }

        // D/E buttons (skip toggles)
        for (pi, &btn_id) in self.driver_btn_ids.iter().enumerate() {
            if self.param_info.get(pi).map(|i| i.is_toggle).unwrap_or(false) { continue; }
            if id == btn_id {
                return vec![PanelAction::GenDriverToggle(pi)];
            }
        }
        for (pi, &btn_id) in self.envelope_btn_ids.iter().enumerate() {
            if self.param_info.get(pi).map(|i| i.is_toggle).unwrap_or(false) { continue; }
            if id == btn_id {
                return vec![PanelAction::GenEnvelopeToggle(pi)];
            }
        }

        // Driver config buttons
        for (pi, cfg) in self.driver_config_ids.iter().enumerate() {
            if let Some(ref c) = cfg {
                for (j, &bid) in c.beat_div_btn_ids.iter().enumerate() {
                    if id == bid { return vec![PanelAction::GenDriverConfig(pi, DriverConfigAction::BeatDiv(j))]; }
                }
                if id == c.dot_btn_id { return vec![PanelAction::GenDriverConfig(pi, DriverConfigAction::Dot)]; }
                if id == c.triplet_btn_id { return vec![PanelAction::GenDriverConfig(pi, DriverConfigAction::Triplet)]; }
                for (j, &wid) in c.wave_btn_ids.iter().enumerate() {
                    if id == wid { return vec![PanelAction::GenDriverConfig(pi, DriverConfigAction::Wave(j))]; }
                }
                if id == c.reverse_btn_id { return vec![PanelAction::GenDriverConfig(pi, DriverConfigAction::Reverse)]; }
            }
        }

        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        // Check envelope targets
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
                let slots = [
                    (&c.attack_slider, EnvelopeParam::Attack, ENV_ADR_MAX),
                    (&c.decay_slider, EnvelopeParam::Decay, ENV_ADR_MAX),
                    (&c.sustain_slider, EnvelopeParam::Sustain, ENV_S_MAX),
                    (&c.release_slider, EnvelopeParam::Release, ENV_ADR_MAX),
                ];
                for (slot, (slider, param, max)) in slots.iter().enumerate() {
                    if node_id == slider.track {
                        self.dragging_env_param = pi as i32;
                        self.dragging_env_slot = slot;
                        let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                        return vec![PanelAction::GenEnvParamChanged(pi, *param, norm * max)];
                    }
                }
            }
        }

        // Check param slider tracks (skip toggles)
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self.param_info.get(pi).map(|i| i.is_toggle).unwrap_or(false) { continue; }
            if let Some(ref ids) = slider {
                if node_id == ids.track {
                    self.dragging_param = pi as i32;
                    let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                    let info = &self.param_info[pi];
                    let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                    let val = if info.whole_numbers { val.round() } else { val };
                    return vec![
                        PanelAction::GenParamSnapshot(pi),
                        PanelAction::GenParamChanged(pi, val),
                    ];
                }
            }
        }

        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        // Target bar drag
        if self.dragging_target_param >= 0 {
            let pi = self.dragging_target_param as usize;
            if let Some(ref slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                if let Some(v) = self.state.target_norm.get_mut(pi) { *v = norm; }
                return vec![PanelAction::GenTargetChanged(pi, norm)];
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
                return vec![PanelAction::GenTrimChanged(pi, new_min, new_max)];
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
                return vec![PanelAction::GenEnvParamChanged(pi, param, val)];
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
                return vec![PanelAction::GenParamChanged(pi, val)];
            }
        }

        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_target_param >= 0 { self.dragging_target_param = -1; return Vec::new(); }
        if self.dragging_trim_param >= 0 { self.dragging_trim_param = -1; return Vec::new(); }
        if self.dragging_env_param >= 0 { self.dragging_env_param = -1; return Vec::new(); }
        if self.dragging_param >= 0 {
            let pi = self.dragging_param as usize;
            self.dragging_param = -1;
            return vec![PanelAction::GenParamCommit(pi)];
        }
        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self.param_info.get(pi).map(|i| i.is_toggle).unwrap_or(false) { continue; }
            if let Some(ref ids) = slider {
                if node_id == ids.track {
                    return vec![PanelAction::GenParamRightClick(pi)];
                }
            }
        }
        Vec::new()
    }
}

impl Default for GenParamPanel {
    fn default() -> Self { Self::new() }
}

// ── Helpers ──────────────────────────────────────────────────────

fn format_param_value(val: f32, whole_numbers: bool) -> String {
    if whole_numbers { format!("{}", val as i32) } else { format!("{:.2}", val) }
}

fn toggle_style(on: bool) -> UIStyle {
    if on {
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

fn config_btn_style() -> UIStyle {
    UIStyle {
        bg_color: color::CONFIG_BTN_INACTIVE_C32,
        hover_bg_color: color::CONFIG_BTN_HOVER_C32,
        pressed_bg_color: color::CONFIG_BTN_PRESSED_C32,
        text_color: color::TEXT_DIMMED_C32,
        font_size: FONT_SIZE,
        corner_radius: 1.0,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    fn test_config() -> GenParamConfig {
        GenParamConfig {
            gen_type_name: "Plasma".into(),
            params: vec![
                GenParamInfo { name: "Speed".into(), min: 0.0, max: 10.0, default: 1.0, whole_numbers: false, is_toggle: false },
                GenParamInfo { name: "Invert".into(), min: 0.0, max: 1.0, default: 0.0, whole_numbers: false, is_toggle: true },
                GenParamInfo { name: "Scale".into(), min: 0.1, max: 5.0, default: 1.0, whole_numbers: false, is_toggle: false },
            ],
        }
    }

    #[test]
    fn build_gen_param() {
        let mut tree = UITree::new();
        let mut panel = GenParamPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.gen_type_label_id >= 0);
        assert!(panel.gen_type_btn_id >= 0);
        assert!(panel.slider_ids[0].is_some()); // Speed = slider
        assert!(panel.toggle_ids[1].is_some());  // Invert = toggle
        assert!(panel.slider_ids[2].is_some()); // Scale = slider
        assert!(panel.node_count > 0);
    }

    #[test]
    fn handle_click_gen_type() {
        let mut tree = UITree::new();
        let mut panel = GenParamPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let actions = panel.handle_click(panel.gen_type_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::GenTypeClicked));
    }

    #[test]
    fn handle_click_toggle_param() {
        let mut tree = UITree::new();
        let mut panel = GenParamPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let toggle = panel.toggle_ids[1].as_ref().unwrap();
        let actions = panel.handle_click(toggle.button_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::GenParamToggle(1)));
    }

    #[test]
    fn sync_values_updates() {
        let mut tree = UITree::new();
        let mut panel = GenParamPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        tree.clear_dirty();
        panel.sync_values(&mut tree, &[5.0, 1.0, 2.5]);
        assert!(tree.has_dirty());
    }

    #[test]
    fn compute_height_with_driver_expanded() {
        let mut panel = GenParamPanel::new();
        panel.configure(&test_config());

        let base_h = panel.compute_height();
        panel.state.driver_expanded[0] = true;
        let expanded_h = panel.compute_height();

        assert!(expanded_h > base_h);
        assert!((expanded_h - base_h - DRIVER_CONFIG_HEIGHT).abs() < 0.1);
    }
}
