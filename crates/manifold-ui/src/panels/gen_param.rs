use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_slider_shared::*;
use super::{DriverConfigAction, EnvelopeParam, PanelAction};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use manifold_core::LayerId;

// ── Layout constants unique to GenParamPanel ────────────────────

const TOGGLE_BTN_W: f32 = 40.0;
const TOGGLE_BTN_H: f32 = 16.0;
const CHANGE_BTN_W: f32 = 60.0;
const CHANGE_BTN_H: f32 = 16.0;

// ── Card layout constants ────────────────────────────────────────
const HEADER_HEIGHT: f32 = 27.5;
const CHEVRON_W: f32 = 18.0;
const BORDER_W: f32 = 1.0;
const CORNER_RADIUS: f32 = 4.0;
const CARD_BOTTOM_MARGIN: f32 = 6.0;

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
    /// OSC address for this parameter (e.g. "/layer/{id}/gen/generator/tesseract/rotXY").
    /// When present, clicking the param label copies this address to clipboard.
    /// Unity: UIElementBuilder.CopyToClipboardLabel.
    pub osc_address: Option<String>,
    /// When set, an Ableton mapping sub-section is shown below the slider.
    pub ableton_display: Option<AbletonMappingDisplay>,
    /// Ableton trim range (range_min, range_max). When present, trim handles are shown.
    pub ableton_range: Option<(f32, f32)>,
}

#[derive(Debug, Clone)]
pub struct GenStringParamInfo {
    pub name: String,
    pub key: String,
    pub value: String,
    /// If true, clicking this param opens a dropdown instead of text input.
    pub use_dropdown: bool,
}

#[derive(Debug, Clone)]
pub struct GenParamConfig {
    pub gen_type_name: String,
    pub params: Vec<GenParamInfo>,
    pub string_params: Vec<GenStringParamInfo>,
    pub driver_active: Vec<bool>,
    pub envelope_active: Vec<bool>,
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
    _label_id: i32,
    button_id: i32,
}

// ── GenParamPanel ────────────────────────────────────────────────

pub struct GenParamPanel {
    // Configuration
    gen_type_name: String,
    param_info: Vec<GenParamInfo>,
    state: GenParamState,
    /// The layer this panel is displaying gen params for.
    layer_id: Option<LayerId>,

    // Card state
    is_collapsed: bool,
    is_selected: bool,

    // Node IDs — card shell
    border_id: i32,
    inner_bg_id: i32,
    header_bg_id: i32,
    name_label_id: i32,
    change_btn_id: i32,
    chevron_id: i32,

    // Node IDs — per-param (sliders or toggles)
    slider_ids: Vec<Option<SliderNodeIds>>,
    toggle_ids: Vec<Option<ToggleParamIds>>,
    driver_btn_ids: Vec<i32>,
    envelope_btn_ids: Vec<i32>,
    driver_config_ids: Vec<Option<DriverConfigIds>>,
    envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    envelope_random_config_ids: Vec<Option<EnvelopeRandomConfigIds>>,
    trim_ids: Vec<Option<TrimHandleIds>>,
    target_ids: Vec<Option<EnvelopeTargetIds>>,
    envelope_range_ids: Vec<Option<TrimHandleIds>>,
    ableton_trim_ids: Vec<Option<TrimHandleIds>>,
    ableton_config_ids: Vec<Option<AbletonConfigIds>>,

    // String params (text fields below sliders)
    string_param_info: Vec<GenStringParamInfo>,
    string_param_btn_ids: Vec<i32>,

    // Per-param OSC addresses (for click-to-copy)
    osc_addresses: Vec<Option<String>>,

    copied_flash: CopyToClipboardLabelState,

    // Drag state
    drag: ParamDragState,

    // Cache
    param_cache: Vec<f32>,
    toggle_cache: Vec<bool>,
    label_cache: Vec<Option<String>>,

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
            layer_id: None,
            is_collapsed: false,
            is_selected: false,
            border_id: -1,
            inner_bg_id: -1,
            header_bg_id: -1,
            name_label_id: -1,
            change_btn_id: -1,
            chevron_id: -1,
            slider_ids: Vec::new(),
            toggle_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            envelope_random_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            target_ids: Vec::new(),
            envelope_range_ids: Vec::new(),
            ableton_trim_ids: Vec::new(),
            ableton_config_ids: Vec::new(),
            string_param_info: Vec::new(),
            string_param_btn_ids: Vec::new(),
            osc_addresses: Vec::new(),
            copied_flash: CopyToClipboardLabelState::default(),
            drag: ParamDragState::new(),
            param_cache: Vec::new(),
            toggle_cache: Vec::new(),
            label_cache: Vec::new(),
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
            &config.env_mode,
            &config.env_random_jump,
            &config.env_range_min,
            &config.env_range_max,
            &config.driver_beat_div_idx,
            &config.driver_waveform_idx,
            &config.driver_reversed,
            &config.driver_dotted,
            &config.driver_triplet,
        );
        self.string_param_info = config.string_params.clone();
        self.string_param_btn_ids = vec![-1; config.string_params.len()];
        self.osc_addresses = config
            .params
            .iter()
            .map(|p| p.osc_address.clone())
            .collect();
        self.copied_flash.clear();
        self.slider_ids = vec![None; n];
        self.toggle_ids = Vec::new();
        self.toggle_ids.resize_with(n, || None);
        self.driver_btn_ids = vec![-1; n];
        self.envelope_btn_ids = vec![-1; n];
        self.driver_config_ids = Vec::new();
        self.driver_config_ids.resize_with(n, || None);
        self.envelope_config_ids = Vec::new();
        self.envelope_config_ids.resize_with(n, || None);
        self.envelope_random_config_ids = Vec::new();
        self.envelope_random_config_ids.resize_with(n, || None);
        self.trim_ids = Vec::new();
        self.trim_ids.resize_with(n, || None);
        self.target_ids = Vec::new();
        self.target_ids.resize_with(n, || None);
        self.envelope_range_ids = Vec::new();
        self.envelope_range_ids.resize_with(n, || None);
        self.ableton_trim_ids = Vec::new();
        self.ableton_trim_ids.resize_with(n, || None);
        self.ableton_config_ids = Vec::new();
        self.ableton_config_ids.resize_with(n, || None);
        self.param_cache = vec![f32::NAN; n];
        self.toggle_cache = vec![false; n];
        self.label_cache = vec![None; n];
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    pub fn state_mut(&mut self) -> &mut GenParamState {
        &mut self.state
    }
    pub fn is_dragging(&self) -> bool {
        self.drag.is_dragging()
    }

    pub fn compute_height(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed {
            for (i, info) in self.param_info.iter().enumerate() {
                if info.is_toggle {
                    h += ROW_HEIGHT + ROW_SPACING;
                } else {
                    h += ROW_HEIGHT + ROW_SPACING;
                    if self
                        .state
                        .mod_state
                        .driver_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                    {
                        h += DRIVER_CONFIG_HEIGHT;
                    }
                    if self
                        .state
                        .mod_state
                        .envelope_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                    {
                        h += ENV_RANDOM_CONFIG_HEIGHT;
                        let env_mode = self
                            .state
                            .mod_state
                            .env_mode
                            .get(i)
                            .copied()
                            .unwrap_or(EnvelopeMode::Adsr);
                        if env_mode == EnvelopeMode::Adsr {
                            h += ENV_CONFIG_HEIGHT;
                        }
                    }
                    if info.ableton_display.is_some() {
                        h += ABL_CONFIG_HEIGHT;
                    }
                }
            }
            // String param rows (text fields)
            for _ in &self.string_param_info {
                h += ROW_HEIGHT + ROW_SPACING;
            }
            if !self.param_info.is_empty() || !self.string_param_info.is_empty() {
                h += PADDING;
            }
        }
        h + CARD_BOTTOM_MARGIN
    }

    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    /// Returns the Ableton label for `param_idx`, if that param is currently mapped.
    pub fn param_has_ableton_mapping(&self, param_idx: usize) -> bool {
        self.param_info
            .get(param_idx)
            .is_some_and(|p| p.ableton_display.is_some())
    }
    pub fn set_collapsed(&mut self, v: bool) {
        self.is_collapsed = v;
    }
    pub fn is_selected(&self) -> bool {
        self.is_selected
    }

    /// Update selection visual (border color) without a full rebuild.
    pub fn update_selection_visual(&mut self, tree: &mut UITree, selected: bool) {
        if selected == self.is_selected {
            return;
        }
        self.is_selected = selected;
        if self.border_id >= 0 {
            let border_color = if selected {
                color::SELECTED_BORDER
            } else {
                color::GEN_CARD_BORDER_C32
            };
            tree.set_style(
                self.border_id as u32,
                UIStyle {
                    bg_color: border_color,
                    corner_radius: CORNER_RADIUS,
                    ..UIStyle::default()
                },
            );
        }
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.toggle_cache.iter_mut().for_each(|v| *v = false);
        self.label_cache.iter_mut().for_each(|v| *v = None);

        let total_h = self.compute_height() - CARD_BOTTOM_MARGIN;

        // ── Card shell ──
        let border_color = if self.is_selected {
            color::SELECTED_BORDER
        } else {
            color::GEN_CARD_BORDER_C32
        };
        self.border_id = tree.add_panel(
            -1,
            rect.x,
            rect.y,
            rect.width,
            total_h,
            UIStyle {
                bg_color: border_color,
                corner_radius: CORNER_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.border_id as u32, UIFlags::INTERACTIVE);

        let inner_x = rect.x + BORDER_W;
        let inner_y = rect.y + BORDER_W;
        let inner_w = rect.width - BORDER_W * 2.0;
        let inner_h = total_h - BORDER_W * 2.0;
        self.inner_bg_id = tree.add_panel(
            -1,
            inner_x,
            inner_y,
            inner_w,
            inner_h,
            UIStyle {
                bg_color: color::GEN_CARD_INNER_BG_C32,
                corner_radius: CORNER_RADIUS - BORDER_W,
                ..UIStyle::default()
            },
        ) as i32;

        // ── Header ──
        self.header_bg_id = tree.add_panel(
            -1,
            inner_x,
            inner_y,
            inner_w,
            HEADER_HEIGHT,
            UIStyle {
                bg_color: color::GEN_CARD_HEADER_BG_C32,
                corner_radius: CORNER_RADIUS - BORDER_W,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.header_bg_id as u32, UIFlags::INTERACTIVE);

        let gen_name = self.gen_type_name.clone();

        // Header layout (right-to-left): [Name] ... [Change] [Chevron]
        let chevron_x = inner_x + inner_w - CHEVRON_W;
        let change_x = chevron_x - CHANGE_BTN_W - GAP;
        let name_x = inner_x + PADDING;
        let name_w = change_x - name_x - GAP;

        self.name_label_id = tree.add_label(
            -1,
            name_x,
            inner_y,
            name_w,
            HEADER_HEIGHT,
            &gen_name,
            UIStyle {
                text_color: color::GEN_CARD_HEADER_NAME_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        self.change_btn_id = tree.add_button(
            -1,
            change_x,
            inner_y + (HEADER_HEIGHT - CHANGE_BTN_H) * 0.5,
            CHANGE_BTN_W,
            CHANGE_BTN_H,
            UIStyle {
                bg_color: color::CONFIG_BG_C32,
                hover_bg_color: color::GEN_CARD_HEADER_HOVER_C32,
                pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                corner_radius: 2.0,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "Change",
        ) as i32;

        let chevron_text = if self.is_collapsed {
            "\u{25B6}"
        } else {
            "\u{25BC}"
        };
        self.chevron_id = tree.add_button(
            -1,
            chevron_x,
            inner_y,
            CHEVRON_W,
            HEADER_HEIGHT,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            chevron_text,
        ) as i32;

        // ── Params (if not collapsed) ──
        if !self.is_collapsed && !self.param_info.is_empty() {
            let content_w = inner_w - PADDING * 2.0;
            let cx = inner_x + PADDING;
            let mut cy = inner_y + HEADER_HEIGHT;
            let slider_w = content_w - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 2.0;

            for i in 0..self.param_info.len() {
                let info = self.param_info[i].clone();

                if info.is_toggle {
                    // Toggle row
                    let label_id = tree.add_label(
                        -1,
                        cx,
                        cy,
                        content_w - TOGGLE_BTN_W - GAP,
                        ROW_HEIGHT,
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
                        -1,
                        cx + content_w - TOGGLE_BTN_W,
                        cy + (ROW_HEIGHT - TOGGLE_BTN_H) * 0.5,
                        TOGGLE_BTN_W,
                        TOGGLE_BTN_H,
                        toggle_btn_style(on),
                        if on { "ON" } else { "OFF" },
                    ) as i32;

                    // Make toggle label interactive for click-to-copy OSC address
                    if self.osc_addresses.get(i).and_then(|a| a.as_ref()).is_some() && label_id >= 0
                    {
                        tree.set_flag(label_id as u32, UIFlags::INTERACTIVE);
                    }

                    self.toggle_ids[i] = Some(ToggleParamIds {
                        _label_id: label_id,
                        button_id,
                    });
                    self.toggle_cache[i] = on;
                    cy += ROW_HEIGHT + ROW_SPACING;
                } else {
                    // Slider row
                    let norm = BitmapSlider::value_to_normalized(info.default, info.min, info.max);
                    let val_text = format_param_value(
                        info.default,
                        info.min,
                        info.whole_numbers,
                        info.value_labels.as_deref(),
                    );
                    let slider_rect = Rect::new(cx, cy, slider_w, ROW_HEIGHT);
                    self.slider_ids[i] = Some(BitmapSlider::build(
                        tree,
                        -1,
                        slider_rect,
                        Some(&info.name),
                        norm,
                        &val_text,
                        &SliderColors::gen_param(),
                        FONT_SIZE,
                        crate::slider::DEFAULT_LABEL_WIDTH,
                    ));

                    // Make label interactive for Ableton mapping + OSC address copy
                    if let Some(ids) = &self.slider_ids[i]
                        && ids.label >= 0
                    {
                        tree.set_flag(ids.label as u32, UIFlags::INTERACTIVE);
                    }

                    // Trim handles (if driver expanded)
                    if self
                        .state
                        .mod_state
                        .driver_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                        && let Some(ref slider) = self.slider_ids[i]
                    {
                        self.trim_ids[i] = Some(build_trim_handles(
                            tree,
                            slider.track as i32,
                            slider.track_rect,
                            &self.state.mod_state,
                            i,
                        ));
                    }

                    // Envelope target or range handles
                    if self
                        .state
                        .mod_state
                        .envelope_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                        && let Some(ref slider) = self.slider_ids[i]
                    {
                        let env_mode = self
                            .state
                            .mod_state
                            .env_mode
                            .get(i)
                            .copied()
                            .unwrap_or(EnvelopeMode::Adsr);
                        if env_mode == EnvelopeMode::Random {
                            self.envelope_range_ids[i] = Some(build_envelope_range_handles(
                                tree,
                                slider.track as i32,
                                slider.track_rect,
                                &self.state.mod_state,
                                i,
                            ));
                        } else {
                            self.target_ids[i] = Some(build_envelope_target(
                                tree,
                                slider.track as i32,
                                slider.track_rect,
                                &self.state.mod_state,
                                i,
                            ));
                        }
                    }

                    // Ableton trim handles
                    if let Some((amin, amax)) = self.param_info[i].ableton_range
                        && let Some(ref slider) = self.slider_ids[i]
                    {
                        self.ableton_trim_ids[i] = Some(build_trim_handles_explicit(
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

                    // D/E buttons
                    let btn_x = cx + slider_w + DE_BUTTON_GAP;
                    let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;

                    let env_active = self
                        .state
                        .mod_state
                        .envelope_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false);
                    self.envelope_btn_ids[i] = tree.add_button(
                        -1,
                        btn_x,
                        btn_y,
                        DE_BUTTON_SIZE,
                        DE_BUTTON_SIZE,
                        de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
                        "E",
                    ) as i32;

                    let drv_active = self
                        .state
                        .mod_state
                        .driver_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false);
                    self.driver_btn_ids[i] = tree.add_button(
                        -1,
                        btn_x + DE_BUTTON_SIZE + DE_BUTTON_GAP,
                        btn_y,
                        DE_BUTTON_SIZE,
                        DE_BUTTON_SIZE,
                        de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
                        "\u{2192}",
                    ) as i32;

                    cy += ROW_HEIGHT + ROW_SPACING;

                    let config_w = content_w; // full width (no D/E button reservation)

                    // Envelope config
                    if self
                        .state
                        .mod_state
                        .envelope_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                    {
                        let env_mode = self
                            .state
                            .mod_state
                            .env_mode
                            .get(i)
                            .copied()
                            .unwrap_or(EnvelopeMode::Adsr);
                        // Always build the random config buttons
                        self.envelope_random_config_ids[i] = Some(build_envelope_random_config(
                            tree,
                            -1,
                            cx,
                            cy,
                            config_w,
                            &self.state.mod_state,
                            i,
                        ));
                        cy += ENV_RANDOM_CONFIG_HEIGHT;
                        // Only show ADSR sliders when in ADSR mode
                        if env_mode == EnvelopeMode::Adsr {
                            self.envelope_config_ids[i] = Some(build_envelope_config(
                                tree,
                                -1,
                                cx,
                                cy,
                                config_w,
                                &self.state.mod_state,
                                i,
                            ));
                            cy += ENV_CONFIG_HEIGHT;
                        }
                    }

                    // Driver config
                    if self
                        .state
                        .mod_state
                        .driver_expanded
                        .get(i)
                        .copied()
                        .unwrap_or(false)
                    {
                        self.driver_config_ids[i] = Some(build_driver_config(
                            tree,
                            -1,
                            cx,
                            cy,
                            config_w,
                            &self.state.mod_state,
                            i,
                            FONT_SIZE,
                        ));
                        cy += DRIVER_CONFIG_HEIGHT;
                    }

                    // Ableton config drawer (auto-shows when mapping exists)
                    if let Some(ref display) = self.param_info[i].ableton_display {
                        self.ableton_config_ids[i] =
                            Some(build_ableton_config(tree, -1, cx, cy, config_w, display));
                        cy += ABL_CONFIG_HEIGHT;
                    }
                }
            }

            // ── String param rows (clickable text fields) ──
            for (si, sp) in self.string_param_info.iter().enumerate() {
                let display = if sp.value.is_empty() {
                    format!("{}: (empty)", sp.name)
                } else {
                    format!("{}: {}", sp.name, sp.value)
                };
                self.string_param_btn_ids[si] = tree.add_button(
                    -1,
                    cx,
                    cy,
                    content_w,
                    ROW_HEIGHT,
                    UIStyle {
                        bg_color: color::INSPECTOR_BG,
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        corner_radius: 2.0,
                        ..UIStyle::default()
                    },
                    &display,
                ) as i32;
                cy += ROW_HEIGHT + ROW_SPACING;
            }
        } // end if !self.is_collapsed

        self.node_count = tree.count() - self.first_node;
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_values(&mut self, tree: &mut UITree, values: &[f32]) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| self.find_label_name(label_id))
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        for (i, &val) in values.iter().enumerate().take(self.param_info.len()) {
            let info = &self.param_info[i];

            // Label dirty-check
            if !info.is_toggle {
                let new_label = Some(info.name.clone());
                if self.label_cache[i] != new_label {
                    self.label_cache[i] = new_label;
                    if let Some(ref ids) = self.slider_ids[i]
                        && ids.label >= 0
                    {
                        tree.set_text(ids.label as u32, &info.name);
                    }
                }
            }

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
                    let text = format_param_value(
                        val,
                        info.min,
                        info.whole_numbers,
                        info.value_labels.as_deref(),
                    );
                    BitmapSlider::update_value(tree, ids, norm, &text);
                }
            }
        }
    }

    /// Find the original param name for a label node ID (slider or toggle).
    fn find_label_name(&self, label_id: u32) -> String {
        for (pi, s) in self.slider_ids.iter().enumerate() {
            if let Some(ids) = s
                && ids.label >= 0
                && ids.label as u32 == label_id
            {
                return self
                    .param_info
                    .get(pi)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
            }
        }
        for (pi, t) in self.toggle_ids.iter().enumerate() {
            if let Some(ids) = t
                && ids._label_id >= 0
                && ids._label_id as u32 == label_id
            {
                return self
                    .param_info
                    .get(pi)
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
            }
        }
        String::new()
    }

    pub fn set_layer_id(&mut self, id: Option<LayerId>) {
        self.layer_id = id;
    }

    /// Get string param info for text input anchoring.
    pub fn string_param(&self, index: usize) -> Option<&GenStringParamInfo> {
        self.string_param_info.get(index)
    }

    /// Get the screen-space rect of a string param button for text input anchoring.
    pub fn string_param_rect(&self, tree: &UITree, index: usize) -> Option<Rect> {
        self.string_param_btn_ids
            .get(index)
            .filter(|&&id| id >= 0)
            .map(|&id| tree.get_bounds(id as u32))
    }

    /// Update a string param value and its display text.
    pub fn sync_string_param(&mut self, tree: &mut UITree, index: usize, value: &str) {
        if let Some(sp) = self.string_param_info.get_mut(index) {
            sp.value = value.to_string();
            if let Some(&btn_id) = self.string_param_btn_ids.get(index)
                && btn_id >= 0
            {
                let display = if value.is_empty() {
                    format!("{}: (empty)", sp.name)
                } else {
                    format!("{}: {}", sp.name, value)
                };
                tree.set_text(btn_id as u32, &display);
            }
        }
    }

    pub fn sync_gen_type_name(&mut self, tree: &mut UITree, name: &str) {
        self.gen_type_name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&mut self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;

        // Chevron → collapse/expand
        if id == self.chevron_id {
            return vec![PanelAction::GenCollapseToggle];
        }

        // Change button → open type picker
        if id == self.change_btn_id {
            return vec![PanelAction::GenTypeClicked(self.layer_id.clone())];
        }

        // Card click (header bg, name, border) → select the card
        if id == self.header_bg_id || id == self.name_label_id || id == self.border_id {
            return vec![PanelAction::GenCardClicked];
        }

        // Toggle buttons
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && id == t.button_id
            {
                return vec![PanelAction::GenParamToggle(pi)];
            }
        }

        // D/E buttons (skip toggles)
        for (pi, &btn_id) in self.driver_btn_ids.iter().enumerate() {
            if self
                .param_info
                .get(pi)
                .map(|i| i.is_toggle)
                .unwrap_or(false)
            {
                continue;
            }
            if id == btn_id {
                return vec![PanelAction::GenDriverToggle(pi)];
            }
        }
        for (pi, &btn_id) in self.envelope_btn_ids.iter().enumerate() {
            if self
                .param_info
                .get(pi)
                .map(|i| i.is_toggle)
                .unwrap_or(false)
            {
                continue;
            }
            if id == btn_id {
                return vec![PanelAction::GenEnvelopeToggle(pi)];
            }
        }

        // Param label click → copy OSC address to clipboard (slider labels)
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let Some(ids) = slider
                && ids.label >= 0
                && id == ids.label
                && let Some(addr) = self.osc_addresses.get(pi).and_then(|a| a.clone())
            {
                self.copied_flash.trigger(ids.label as u32);
                return vec![PanelAction::CopyOscAddress(addr)];
            }
        }

        // Param label click → copy OSC address to clipboard (toggle labels)
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && t._label_id >= 0
                && id == t._label_id
                && let Some(addr) = self.osc_addresses.get(pi).and_then(|a| a.clone())
            {
                self.copied_flash.trigger(t._label_id as u32);
                return vec![PanelAction::CopyOscAddress(addr)];
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

        // Envelope random config buttons (mode toggle, jump toggle)
        for (pi, cfg) in self.envelope_random_config_ids.iter().enumerate() {
            if let Some(c) = cfg {
                if id == c.mode_btn_id {
                    return vec![PanelAction::GenEnvModeToggle(pi)];
                }
                if id == c.jump_btn_id {
                    return vec![PanelAction::GenEnvRandomJumpToggle(pi)];
                }
            }
        }

        // Ableton config buttons
        if let Some((pi, AbletonConfigClick::Invert)) =
            check_ableton_config_click(id, &self.ableton_config_ids)
        {
            return vec![PanelAction::AbletonGenInvertToggle(pi)];
        }

        // String param buttons → open text input or dropdown
        for (si, &btn_id) in self.string_param_btn_ids.iter().enumerate() {
            if id == btn_id {
                if self
                    .string_param_info
                    .get(si)
                    .is_some_and(|sp| sp.use_dropdown)
                {
                    return vec![PanelAction::GenStringParamDropdownClicked(si)];
                }
                return vec![PanelAction::GenStringParamClicked(si)];
            }
        }

        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        // Check envelope range handles (Random mode)
        for (pi, range) in self.envelope_range_ids.iter().enumerate() {
            if let Some(t) = range {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_range_param = pi as i32;
                    self.drag.dragging_range_is_min = true;
                    return vec![PanelAction::GenEnvRangeSnapshot(pi)];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_range_param = pi as i32;
                    self.drag.dragging_range_is_min = false;
                    return vec![PanelAction::GenEnvRangeSnapshot(pi)];
                }
            }
        }

        // Check envelope targets (ADSR mode)
        for (pi, target) in self.target_ids.iter().enumerate() {
            if let Some(t) = target
                && node_id as i32 == t.target_bar_id
            {
                self.drag.dragging_target_param = pi as i32;
                return vec![PanelAction::GenTargetSnapshot(pi)];
            }
        }

        // Check trim bars
        for (pi, trim) in self.trim_ids.iter().enumerate() {
            if let Some(t) = trim {
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

        // Check Ableton trim bars
        for (pi, trim) in self.ableton_trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_ableton_trim_param = pi as i32;
                    self.drag.dragging_ableton_trim_is_min = true;
                    return vec![PanelAction::AbletonGenTrimSnapshot(pi)];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_ableton_trim_param = pi as i32;
                    self.drag.dragging_ableton_trim_is_min = false;
                    return vec![PanelAction::AbletonGenTrimSnapshot(pi)];
                }
            }
        }

        // Check ADSR slider tracks
        for (pi, env_cfg) in self.envelope_config_ids.iter().enumerate() {
            if let Some(c) = env_cfg {
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
                        return vec![
                            PanelAction::GenEnvParamSnapshot(pi),
                            PanelAction::GenEnvParamChanged(pi, *param, norm * max),
                        ];
                    }
                }
            }
        }

        // Check param slider tracks (skip toggles)
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self
                .param_info
                .get(pi)
                .map(|i| i.is_toggle)
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(ids) = slider
                && node_id == ids.track
            {
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

        Vec::new()
    }

    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        // Range handle drag
        if self.drag.dragging_range_param >= 0 {
            let pi = self.drag.dragging_range_param as usize;
            if let Some(slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                let rmin = self
                    .state
                    .mod_state
                    .env_range_min
                    .get(pi)
                    .copied()
                    .unwrap_or(0.0);
                let rmax = self
                    .state
                    .mod_state
                    .env_range_max
                    .get(pi)
                    .copied()
                    .unwrap_or(1.0);
                let (new_min, new_max) = if self.drag.dragging_range_is_min {
                    (norm.min(rmax), rmax)
                } else {
                    (rmin, norm.max(rmin))
                };
                if let Some(v) = self.state.mod_state.env_range_min.get_mut(pi) {
                    *v = new_min;
                }
                if let Some(v) = self.state.mod_state.env_range_max.get_mut(pi) {
                    *v = new_max;
                }

                if let Some(t) = self.envelope_range_ids.get(pi).and_then(|t| t.as_ref()) {
                    let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = slider.track_rect.x + OVERLAY_INSET;
                    let fill_x = base_x + new_min * usable;
                    let fill_w = (new_max - new_min) * usable;
                    let fill_h = slider.track_rect.height - OVERLAY_INSET * 2.0;
                    tree.set_bounds(
                        t.fill_id as u32,
                        Rect::new(fill_x, slider.track_rect.y + OVERLAY_INSET, fill_w, fill_h),
                    );
                    tree.set_bounds(
                        t.min_bar_id as u32,
                        Rect::new(
                            base_x + new_min * usable - TRIM_BAR_W * 0.5,
                            slider.track_rect.y,
                            TRIM_BAR_W,
                            slider.track_rect.height,
                        ),
                    );
                    tree.set_bounds(
                        t.max_bar_id as u32,
                        Rect::new(
                            base_x + new_max * usable - TRIM_BAR_W * 0.5,
                            slider.track_rect.y,
                            TRIM_BAR_W,
                            slider.track_rect.height,
                        ),
                    );
                }

                return vec![PanelAction::GenEnvRangeChanged(pi, new_min, new_max)];
            }
        }

        // Target bar drag — update state, reposition bar node, dispatch action
        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            if let Some(slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                if let Some(v) = self.state.mod_state.target_norm.get_mut(pi) {
                    *v = norm;
                }

                // Visual update: reposition target bar node in the tree
                if let Some(t) = self.target_ids.get(pi).and_then(|t| t.as_ref()) {
                    let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = slider.track_rect.x + OVERLAY_INSET;
                    let bar_x = base_x + norm * usable - TARGET_BAR_W * 0.5;
                    let bar_h = slider.track_rect.height + 4.0;
                    let bar_y = slider.track_rect.y - 2.0;
                    tree.set_bounds(
                        t.target_bar_id as u32,
                        Rect::new(bar_x, bar_y, TARGET_BAR_W, bar_h),
                    );
                }

                return vec![PanelAction::GenTargetChanged(pi, norm)];
            }
        }

        // Trim bar drag — update state, reposition bar nodes, dispatch action
        if self.drag.dragging_trim_param >= 0 {
            let pi = self.drag.dragging_trim_param as usize;
            if let Some(slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                let tmin = self
                    .state
                    .mod_state
                    .trim_min
                    .get(pi)
                    .copied()
                    .unwrap_or(0.0);
                let tmax = self
                    .state
                    .mod_state
                    .trim_max
                    .get(pi)
                    .copied()
                    .unwrap_or(1.0);
                let (new_min, new_max) = if self.drag.dragging_trim_is_min {
                    (norm.min(tmax), tmax)
                } else {
                    (tmin, norm.max(tmin))
                };
                if let Some(v) = self.state.mod_state.trim_min.get_mut(pi) {
                    *v = new_min;
                }
                if let Some(v) = self.state.mod_state.trim_max.get_mut(pi) {
                    *v = new_max;
                }

                // Visual update: reposition trim bar nodes in the tree
                if let Some(t) = self.trim_ids.get(pi).and_then(|t| t.as_ref()) {
                    let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = slider.track_rect.x + OVERLAY_INSET;
                    let fill_x = base_x + new_min * usable;
                    let fill_w = (new_max - new_min) * usable;
                    let fill_h = slider.track_rect.height - OVERLAY_INSET * 2.0;
                    tree.set_bounds(
                        t.fill_id as u32,
                        Rect::new(fill_x, slider.track_rect.y + OVERLAY_INSET, fill_w, fill_h),
                    );
                    tree.set_bounds(
                        t.min_bar_id as u32,
                        Rect::new(
                            base_x + new_min * usable - TRIM_BAR_W * 0.5,
                            slider.track_rect.y,
                            TRIM_BAR_W,
                            slider.track_rect.height,
                        ),
                    );
                    tree.set_bounds(
                        t.max_bar_id as u32,
                        Rect::new(
                            base_x + new_max * usable - TRIM_BAR_W * 0.5,
                            slider.track_rect.y,
                            TRIM_BAR_W,
                            slider.track_rect.height,
                        ),
                    );
                }

                return vec![PanelAction::GenTrimChanged(pi, new_min, new_max)];
            }
        }

        // Ableton trim bar drag
        if self.drag.dragging_ableton_trim_param >= 0 {
            let pi = self.drag.dragging_ableton_trim_param as usize;
            if let Some(slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref())
                && let Some((cur_min, cur_max)) = self.param_info[pi].ableton_range
            {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                let (new_min, new_max) = if self.drag.dragging_ableton_trim_is_min {
                    (norm.clamp(0.0, cur_max), cur_max)
                } else {
                    (cur_min, norm.clamp(cur_min, 1.0))
                };
                self.param_info[pi].ableton_range = Some((new_min, new_max));

                if let Some(t) = self.ableton_trim_ids.get(pi).and_then(|t| t.as_ref()) {
                    let usable = slider.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = slider.track_rect.x + OVERLAY_INSET;
                    let fill_x = base_x + new_min * usable;
                    let fill_w = (new_max - new_min) * usable;
                    let fill_h = slider.track_rect.height - OVERLAY_INSET * 2.0;
                    tree.set_bounds(
                        t.fill_id as u32,
                        Rect::new(fill_x, slider.track_rect.y + OVERLAY_INSET, fill_w, fill_h),
                    );
                    tree.set_bounds(
                        t.min_bar_id as u32,
                        Rect::new(
                            base_x + new_min * usable - TRIM_BAR_W * 0.5,
                            slider.track_rect.y,
                            TRIM_BAR_W,
                            slider.track_rect.height,
                        ),
                    );
                    tree.set_bounds(
                        t.max_bar_id as u32,
                        Rect::new(
                            base_x + new_max * usable - TRIM_BAR_W * 0.5,
                            slider.track_rect.y,
                            TRIM_BAR_W,
                            slider.track_rect.height,
                        ),
                    );
                }

                return vec![PanelAction::AbletonGenTrimChanged(pi, new_min, new_max)];
            }
        }

        // ADSR drag
        if self.drag.dragging_env_param >= 0 {
            let pi = self.drag.dragging_env_param as usize;
            if let Some(cfg) = self.envelope_config_ids.get(pi).and_then(|c| c.as_ref()) {
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
            if let Some(ids) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let info = &self.param_info[pi];
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                let val = if info.whole_numbers { val.round() } else { val };
                let display_norm = BitmapSlider::value_to_normalized(val, info.min, info.max);
                let text = format_param_value(
                    val,
                    info.min,
                    info.whole_numbers,
                    info.value_labels.as_deref(),
                );
                BitmapSlider::update_value(tree, ids, display_norm, &text);
                self.param_cache[pi] = val;
                return vec![PanelAction::GenParamChanged(pi, val)];
            }
        }

        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.drag.dragging_range_param >= 0 {
            let pi = self.drag.dragging_range_param as usize;
            self.drag.dragging_range_param = -1;
            return vec![PanelAction::GenEnvRangeCommit(pi)];
        }
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
        if self.drag.dragging_ableton_trim_param >= 0 {
            let pi = self.drag.dragging_ableton_trim_param as usize;
            self.drag.dragging_ableton_trim_param = -1;
            return vec![PanelAction::AbletonGenTrimCommit(pi)];
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
        let id = node_id as i32;

        // Header right-click → context menu for copy/paste
        if id == self.header_bg_id
            || id == self.name_label_id
            || id == self.border_id
            || id == self.inner_bg_id
        {
            return vec![PanelAction::GenCardRightClicked];
        }

        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self
                .param_info
                .get(pi)
                .map(|i| i.is_toggle)
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(ids) = slider {
                // Right-click slider track → reset to default
                if node_id == ids.track {
                    let default = self.param_info.get(pi).map(|i| i.default).unwrap_or(0.0);
                    return vec![PanelAction::GenParamRightClick(pi, default)];
                }
                // Right-click label → map to macro
                if ids.label >= 0 && node_id == ids.label as u32 {
                    return vec![PanelAction::GenParamLabelRightClick(pi)];
                }
            }
        }
        Vec::new()
    }
}

impl Default for GenParamPanel {
    fn default() -> Self {
        Self::new()
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
                GenParamInfo {
                    name: "Speed".into(),
                    min: 0.0,
                    max: 10.0,
                    default: 1.0,
                    whole_numbers: false,
                    is_toggle: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
                GenParamInfo {
                    name: "Invert".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    whole_numbers: false,
                    is_toggle: true,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
                GenParamInfo {
                    name: "Scale".into(),
                    min: 0.1,
                    max: 5.0,
                    default: 1.0,
                    whole_numbers: false,
                    is_toggle: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
            ],
            string_params: vec![],
            driver_active: vec![false; 3],
            envelope_active: vec![false; 3],
            trim_min: vec![0.0; 3],
            trim_max: vec![1.0; 3],
            target_norm: vec![1.0; 3],
            env_attack: vec![0.0; 3],
            env_decay: vec![0.0; 3],
            env_sustain: vec![0.0; 3],
            env_release: vec![0.0; 3],
            env_mode: vec![EnvelopeMode::Adsr; 3],
            env_random_jump: vec![false; 3],
            env_range_min: vec![0.0; 3],
            env_range_max: vec![1.0; 3],
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

        assert!(panel.border_id >= 0);
        assert!(panel.name_label_id >= 0);
        assert!(panel.chevron_id >= 0);
        assert!(panel.slider_ids[0].is_some()); // Speed = slider
        assert!(panel.toggle_ids[1].is_some()); // Invert = toggle
        assert!(panel.slider_ids[2].is_some()); // Scale = slider
        assert!(panel.node_count > 0);
    }

    #[test]
    fn handle_click_gen_type() {
        let mut tree = UITree::new();
        let mut panel = GenParamPanel::new();
        panel.configure(&test_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        // Clicking the Change button opens the type picker
        let actions = panel.handle_click(panel.change_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::GenTypeClicked(_)));

        // Clicking the name label selects the card
        let actions = panel.handle_click(panel.name_label_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::GenCardClicked));
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
