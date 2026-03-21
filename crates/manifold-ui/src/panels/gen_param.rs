use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::{PanelAction, DriverConfigAction, EnvelopeParam};
use super::param_slider_shared::*;

// ── Layout constants unique to GenParamPanel ────────────────────

const GEN_TYPE_ROW_H: f32 = 22.0;
const SECTION_LABEL_H: f32 = 18.0;
const DIVIDER_H: f32 = 1.0;
const TOGGLE_BTN_W: f32 = 40.0;
const TOGGLE_BTN_H: f32 = 16.0;

const CHANGE_BTN_W: f32 = 100.0;

// ── Panel-specific colors (imported from color module) ───────────

use crate::color::{GEN_TYPE_HOVER, GEN_TYPE_LABEL as GEN_TYPE_LABEL_COLOR};

// ── Data types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GenParamInfo {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    pub is_toggle: bool,
    /// Named value labels for discrete params (e.g., ["Classic", "Rings", "Diamond"]).
    /// When present, the slider displays the label instead of a numeric value.
    /// Unity: ParamDef.valueLabels → GeneratorDefinitionRegistry.FormatValue().
    pub value_labels: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct GenParamConfig {
    pub gen_type_name: String,
    pub params: Vec<GenParamInfo>,
    pub driver_active: Vec<bool>,
    pub envelope_active: Vec<bool>,
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

/// Per-parameter expansion state for generator params.
/// Wraps ParamModState for shared modulation state.
pub struct GenParamState {
    /// Shared per-param modulation state (driver/envelope expansion, trim, target, ADSR, driver config).
    pub mod_state: ParamModState,
}

impl GenParamState {
    pub fn new(param_count: usize) -> Self {
        Self {
            mod_state: ParamModState::allocate(param_count),
        }
    }
}

// ── Internal node ID structs ─────────────────────────────────────

struct ToggleParamIds {
    label_id: i32,
    button_id: i32,
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
    drag: ParamDragState,

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
            drag: ParamDragState::new(),
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
        self.state.mod_state.sync_from_config(
            n,
            &config.driver_active,
            &config.envelope_active,
            &config.trim_min,
            &config.trim_max,
            &config.target_norm,
            &config.env_attack,
            &config.env_decay,
            &config.env_sustain,
            &config.env_release,
            &config.driver_beat_div_idx,
            &config.driver_waveform_idx,
            &config.driver_reversed,
            &config.driver_dotted,
            &config.driver_triplet,
        );
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
        self.drag.is_dragging()
    }

    pub fn compute_height(&self) -> f32 {
        let mut h = GEN_TYPE_ROW_H + DIVIDER_H + SECTION_LABEL_H;
        for (i, info) in self.param_info.iter().enumerate() {
            if info.is_toggle {
                h += ROW_HEIGHT + ROW_SPACING;
            } else {
                h += ROW_HEIGHT + ROW_SPACING;
                if self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
                    h += DRIVER_CONFIG_HEIGHT;
                }
                if self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
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
                    toggle_btn_style(on),
                    if on { "ON" } else { "OFF" },
                ) as i32;

                self.toggle_ids[i] = Some(ToggleParamIds { label_id, button_id });
                self.toggle_cache[i] = on;
                cy += ROW_HEIGHT + ROW_SPACING;
            } else {
                // Slider row
                let norm = BitmapSlider::value_to_normalized(info.default, info.min, info.max);
                let val_text = format_param_value(info.default, info.whole_numbers, info.value_labels.as_deref());
                let slider_rect = Rect::new(cx, cy, slider_w, ROW_HEIGHT);
                self.slider_ids[i] = Some(BitmapSlider::build(
                    tree, -1, slider_rect,
                    Some(&info.name), norm,
                    &val_text, &SliderColors::default_slider(),
                    FONT_SIZE, crate::slider::DEFAULT_LABEL_WIDTH,
                ));

                // Trim handles (if driver expanded)
                if self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
                    if let Some(ref slider) = self.slider_ids[i] {
                        self.trim_ids[i] = Some(build_trim_handles(
                            tree, slider.track as i32, slider.track_rect, &self.state.mod_state, i,
                        ));
                    }
                }

                // Envelope target
                if self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    if let Some(ref slider) = self.slider_ids[i] {
                        self.target_ids[i] = Some(build_envelope_target(
                            tree, slider.track as i32, slider.track_rect, &self.state.mod_state, i,
                        ));
                    }
                }

                // D/E buttons
                let btn_x = cx + slider_w + DE_BUTTON_GAP;
                let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;

                let env_active = self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false);
                self.envelope_btn_ids[i] = tree.add_button(
                    -1, btn_x, btn_y, DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                    de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
                    "E",
                ) as i32;

                let drv_active = self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false);
                self.driver_btn_ids[i] = tree.add_button(
                    -1, btn_x + DE_BUTTON_SIZE + DE_BUTTON_GAP, btn_y,
                    DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                    de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
                    "\u{2192}",
                ) as i32;

                cy += ROW_HEIGHT + ROW_SPACING;

                // Envelope config
                if self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    self.envelope_config_ids[i] = Some(build_envelope_config(tree, -1, cx, cy, slider_w, &self.state.mod_state, i));
                    cy += ENV_CONFIG_HEIGHT;
                }

                // Driver config
                if self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
                    self.driver_config_ids[i] = Some(build_driver_config(tree, -1, cx, cy, slider_w, &self.state.mod_state, i, FONT_SIZE));
                    cy += DRIVER_CONFIG_HEIGHT;
                }
            }
        }

        self.node_count = tree.count() - self.first_node;
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
                        tree.set_style(ids.button_id as u32, toggle_btn_style(on));
                        tree.set_text(ids.button_id as u32, if on { "ON" } else { "OFF" });
                    }
                }
            } else if val != self.param_cache[i] || self.param_cache[i].is_nan() {
                self.param_cache[i] = val;
                if let Some(ref ids) = self.slider_ids[i] {
                    let norm = BitmapSlider::value_to_normalized(val, info.min, info.max);
                    let text = format_param_value(val, info.whole_numbers, info.value_labels.as_deref());
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
        if let Some((pi, result)) = check_driver_config_click(id, &self.driver_config_ids) {
            let action = match result {
                DriverClickResult::BeatDiv(j) => DriverConfigAction::BeatDiv(j),
                DriverClickResult::Dot => DriverConfigAction::Dot,
                DriverClickResult::Triplet => DriverConfigAction::Triplet,
                DriverClickResult::Wave(j) => DriverConfigAction::Wave(j),
                DriverClickResult::Reverse => DriverConfigAction::Reverse,
            };
            return vec![PanelAction::GenDriverConfig(pi, action)];
        }

        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        // Check envelope targets
        for (pi, target) in self.target_ids.iter().enumerate() {
            if let Some(ref t) = target {
                if node_id as i32 == t.target_bar_id {
                    self.drag.dragging_target_param = pi as i32;
                    return vec![PanelAction::GenTargetSnapshot(pi)];
                }
            }
        }

        // Check trim bars
        for (pi, trim) in self.trim_ids.iter().enumerate() {
            if let Some(ref t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = true;
                    return vec![PanelAction::GenTrimSnapshot(pi)];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = false;
                    return vec![PanelAction::GenTrimSnapshot(pi)];
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
                        self.drag.dragging_env_param = pi as i32;
                        self.drag.dragging_env_slot = slot;
                        let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                        return vec![PanelAction::GenEnvParamSnapshot(pi), PanelAction::GenEnvParamChanged(pi, *param, norm * max)];
                    }
                }
            }
        }

        // Check param slider tracks (skip toggles)
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self.param_info.get(pi).map(|i| i.is_toggle).unwrap_or(false) { continue; }
            if let Some(ref ids) = slider {
                if node_id == ids.track {
                    self.drag.dragging_param = pi as i32;
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
        // Target bar drag — update state, reposition bar node, dispatch action
        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            if let Some(ref slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                if let Some(v) = self.state.mod_state.target_norm.get_mut(pi) { *v = norm; }

                // Visual update: reposition target bar node in the tree
                if let Some(ref t) = self.target_ids.get(pi).and_then(|t| t.as_ref()) {
                    let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = slider.track_rect.x + OVERLAY_INSET;
                    let bar_x = base_x + norm * usable - TARGET_BAR_W * 0.5;
                    let bar_h = slider.track_rect.height + 4.0;
                    let bar_y = slider.track_rect.y - 2.0;
                    tree.set_bounds(t.target_bar_id as u32, Rect::new(
                        bar_x, bar_y, TARGET_BAR_W, bar_h,
                    ));
                }

                return vec![PanelAction::GenTargetChanged(pi, norm)];
            }
        }

        // Trim bar drag — update state, reposition bar nodes, dispatch action
        if self.drag.dragging_trim_param >= 0 {
            let pi = self.drag.dragging_trim_param as usize;
            if let Some(ref slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                let tmin = self.state.mod_state.trim_min.get(pi).copied().unwrap_or(0.0);
                let tmax = self.state.mod_state.trim_max.get(pi).copied().unwrap_or(1.0);
                let (new_min, new_max) = if self.drag.dragging_trim_is_min {
                    (norm.min(tmax), tmax)
                } else {
                    (tmin, norm.max(tmin))
                };
                if let Some(v) = self.state.mod_state.trim_min.get_mut(pi) { *v = new_min; }
                if let Some(v) = self.state.mod_state.trim_max.get_mut(pi) { *v = new_max; }

                // Visual update: reposition trim bar nodes in the tree
                if let Some(ref t) = self.trim_ids.get(pi).and_then(|t| t.as_ref()) {
                    let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = slider.track_rect.x + OVERLAY_INSET;
                    let fill_x = base_x + new_min * usable;
                    let fill_w = (new_max - new_min) * usable;
                    let fill_h = slider.track_rect.height - OVERLAY_INSET * 2.0;
                    tree.set_bounds(t.fill_id as u32, Rect::new(
                        fill_x, slider.track_rect.y + OVERLAY_INSET, fill_w, fill_h,
                    ));
                    tree.set_bounds(t.min_bar_id as u32, Rect::new(
                        base_x + new_min * usable - TRIM_BAR_W * 0.5,
                        slider.track_rect.y, TRIM_BAR_W, slider.track_rect.height,
                    ));
                    tree.set_bounds(t.max_bar_id as u32, Rect::new(
                        base_x + new_max * usable - TRIM_BAR_W * 0.5,
                        slider.track_rect.y, TRIM_BAR_W, slider.track_rect.height,
                    ));
                }

                return vec![PanelAction::GenTrimChanged(pi, new_min, new_max)];
            }
        }

        // ADSR drag
        if self.drag.dragging_env_param >= 0 {
            let pi = self.drag.dragging_env_param as usize;
            if let Some(ref cfg) = self.envelope_config_ids.get(pi).and_then(|c| c.as_ref()) {
                let (slider, param, max) = match self.drag.dragging_env_slot {
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
        if self.drag.dragging_param >= 0 {
            let pi = self.drag.dragging_param as usize;
            if let Some(ref ids) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let info = &self.param_info[pi];
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                let val = if info.whole_numbers { val.round() } else { val };
                let text = format_param_value(val, info.whole_numbers, info.value_labels.as_deref());
                BitmapSlider::update_value(tree, ids, norm, &text);
                self.param_cache[pi] = val;
                return vec![PanelAction::GenParamChanged(pi, val)];
            }
        }

        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            self.drag.dragging_target_param = -1;
            return vec![PanelAction::GenTargetCommit(pi)];
        }
        if self.drag.dragging_trim_param >= 0 {
            let pi = self.drag.dragging_trim_param as usize;
            self.drag.dragging_trim_param = -1;
            return vec![PanelAction::GenTrimCommit(pi)];
        }
        if self.drag.dragging_env_param >= 0 {
            let pi = self.drag.dragging_env_param as usize;
            self.drag.dragging_env_param = -1;
            return vec![PanelAction::GenEnvParamCommit(pi)];
        }
        if self.drag.dragging_param >= 0 {
            let pi = self.drag.dragging_param as usize;
            self.drag.dragging_param = -1;
            return vec![PanelAction::GenParamCommit(pi)];
        }
        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self.param_info.get(pi).map(|i| i.is_toggle).unwrap_or(false) { continue; }
            if let Some(ref ids) = slider {
                if node_id == ids.track {
                    let default = self.param_info.get(pi).map(|i| i.default).unwrap_or(0.0);
                    return vec![PanelAction::GenParamRightClick(pi, default)];
                }
            }
        }
        Vec::new()
    }
}

impl Default for GenParamPanel {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    fn test_config() -> GenParamConfig {
        GenParamConfig {
            gen_type_name: "Plasma".into(),
            params: vec![
                GenParamInfo { name: "Speed".into(), min: 0.0, max: 10.0, default: 1.0, whole_numbers: false, is_toggle: false, value_labels: None },
                GenParamInfo { name: "Invert".into(), min: 0.0, max: 1.0, default: 0.0, whole_numbers: false, is_toggle: true, value_labels: None },
                GenParamInfo { name: "Scale".into(), min: 0.1, max: 5.0, default: 1.0, whole_numbers: false, is_toggle: false, value_labels: None },
            ],
            driver_active: vec![false; 3],
            envelope_active: vec![false; 3],
            trim_min: vec![0.0; 3],
            trim_max: vec![1.0; 3],
            target_norm: vec![1.0; 3],
            env_attack: vec![0.0; 3],
            env_decay: vec![0.0; 3],
            env_sustain: vec![0.0; 3],
            env_release: vec![0.0; 3],
            driver_beat_div_idx: vec![-1; 3],
            driver_waveform_idx: vec![-1; 3],
            driver_reversed: vec![false; 3],
            driver_dotted: vec![false; 3],
            driver_triplet: vec![false; 3],
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
        panel.state.mod_state.driver_expanded[0] = true;
        let expanded_h = panel.compute_height();

        assert!(expanded_h > base_h);
        assert!((expanded_h - base_h - DRIVER_CONFIG_HEIGHT).abs() < 0.1);
    }
}
