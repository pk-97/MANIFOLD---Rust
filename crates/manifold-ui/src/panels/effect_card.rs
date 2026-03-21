use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use super::{PanelAction, DriverConfigAction, EnvelopeParam};
use super::param_slider_shared::*;

// ── Layout constants unique to EffectCardPanel ──────────────────

const HEADER_HEIGHT: f32 = 27.5;

const DRAG_HANDLE_W: f32 = 18.0;
const TOGGLE_W: f32 = 30.0;
const CHEVRON_W: f32 = 18.0;
const BADGE_W: f32 = 36.0;
const BADGE_H: f32 = 14.0;
const BADGE_RADIUS: f32 = 7.0;

const BORDER_W: f32 = 1.0;
const CORNER_RADIUS: f32 = 4.0;
const CARD_BOTTOM_MARGIN: f32 = 4.0;

/// Font size for config buttons in effect card (effect uses 8, not FONT_SIZE=10).
const CONFIG_BTN_FONT_SIZE: u16 = 8;

// ── Data types ───────────────────────────────────────────────────

/// Per-parameter configuration info provided by the app layer.
#[derive(Debug, Clone)]
pub struct EffectParamInfo {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    /// Named value labels for discrete params (e.g., ["Horiz", "Vert", "Both"]).
    /// When present, the slider displays the label instead of a numeric value.
    /// Unity: ParamDef.valueLabels → EffectDefinitionRegistry.FormatValue().
    pub value_labels: Option<Vec<String>>,
}

/// Configuration for creating an effect card.
/// Unity: EffectCardState.SyncFromDataModel — all data-derived visual state in one struct.
#[derive(Debug, Clone)]
pub struct EffectCardConfig {
    pub effect_index: usize,
    pub name: String,
    pub enabled: bool,
    pub collapsed: bool,
    pub supports_envelopes: bool,
    pub params: Vec<EffectParamInfo>,
    /// Aggregate: true if ANY param has an active driver.
    pub has_drv: bool,
    /// Aggregate: true if ANY param has an active envelope.
    pub has_env: bool,
    /// Per-param: true if driver exists and is enabled (Unity: driverExpanded[]).
    pub driver_active: Vec<bool>,
    /// Per-param: true if envelope exists and is enabled (Unity: envelopeExpanded[]).
    pub envelope_active: Vec<bool>,
    /// Per-param driver trim min (normalized). Defaults to 0.0.
    pub trim_min: Vec<f32>,
    /// Per-param driver trim max (normalized). Defaults to 1.0.
    pub trim_max: Vec<f32>,
    /// Per-param envelope target (normalized). Defaults to 1.0.
    pub target_norm: Vec<f32>,
    /// Per-param envelope ADSR values (beats).
    pub env_attack: Vec<f32>,
    pub env_decay: Vec<f32>,
    pub env_sustain: Vec<f32>,
    pub env_release: Vec<f32>,
    /// Per-param driver beat division button index (0-10). -1 if no driver.
    pub driver_beat_div_idx: Vec<i32>,
    /// Per-param driver waveform index (0-4). -1 if no driver.
    pub driver_waveform_idx: Vec<i32>,
    /// Per-param driver reversed state.
    pub driver_reversed: Vec<bool>,
    /// Per-param driver dotted modifier active.
    pub driver_dotted: Vec<bool>,
    /// Per-param driver triplet modifier active.
    pub driver_triplet: Vec<bool>,
}

/// Per-parameter expansion and modulation state.
/// Unity: EffectCardState — presenter-owned, single source of truth for
/// all data-derived visual state. Panels read from this.
pub struct EffectCardState {
    /// Aggregate: any param has active driver. Used for DRV badge.
    pub has_drv: bool,
    /// Aggregate: any param has active envelope. Used for ENV badge.
    pub has_env: bool,
    /// Shared per-param modulation state (driver/envelope expansion, trim, target, ADSR, driver config).
    pub mod_state: ParamModState,
}

impl EffectCardState {
    pub fn new(param_count: usize) -> Self {
        Self {
            has_drv: false,
            has_env: false,
            mod_state: ParamModState::allocate(param_count),
        }
    }
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

    // Dirty-check cache (Unity: EffectCardBitmapPanel.cachedEnabled/cachedHasEnv/cachedHasDrv)
    cached_enabled: bool,
    cached_has_env: bool,
    cached_has_drv: bool,

    // Node IDs — header
    header_bg_id: i32,
    drag_icon_id: i32,
    name_label_id: i32,
    toggle_btn_id: i32,
    chevron_btn_id: i32,
    env_badge_bg_id: i32,
    env_badge_text_id: i32,
    drv_badge_bg_id: i32,
    drv_badge_text_id: i32,

    // Node IDs — per-param
    slider_ids: Vec<Option<SliderNodeIds>>,
    driver_btn_ids: Vec<i32>,
    envelope_btn_ids: Vec<i32>,
    driver_config_ids: Vec<Option<DriverConfigIds>>,
    envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    trim_ids: Vec<Option<TrimHandleIds>>,
    target_ids: Vec<Option<EnvelopeTargetIds>>,

    // Drag state
    drag: ParamDragState,

    // Param value cache (NaN = needs sync)
    param_cache: Vec<f32>,

    // Node range
    first_node: usize,
    node_count: usize,

    // Card position (for drag-reorder hit testing)
    card_y: f32,
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
            cached_enabled: true,
            cached_has_env: false,
            cached_has_drv: false,
            border_id: -1,
            inner_bg_id: -1,
            header_bg_id: -1,
            drag_icon_id: -1,
            name_label_id: -1,
            toggle_btn_id: -1,
            chevron_btn_id: -1,
            env_badge_bg_id: -1,
            env_badge_text_id: -1,
            drv_badge_bg_id: -1,
            drv_badge_text_id: -1,
            slider_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            target_ids: Vec::new(),
            drag: ParamDragState::new(),
            param_cache: Vec::new(),
            first_node: 0,
            node_count: 0,
            card_y: 0.0,
        }
    }

    /// Configure with effect metadata. Call before build.
    /// Unity: EffectCardPresenter creates EffectCard with state.SyncFromDataModel().
    /// All data-derived visual state is populated from the config (which was built from
    /// EffectInstance + envelopes + drivers in the app layer).
    pub fn configure(&mut self, config: &EffectCardConfig) {
        self.effect_index = config.effect_index;
        self.effect_name = config.name.clone();
        self.enabled = config.enabled;
        self.is_collapsed = config.collapsed;
        self.supports_envelopes = config.supports_envelopes;
        self.param_info = config.params.clone();

        let n = config.params.len();
        self.state = EffectCardState::new(n);
        // Sync modulation state from config (Unity: SyncFromDataModel)
        self.state.has_drv = config.has_drv;
        self.state.has_env = config.has_env;
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
    pub fn effect_name(&self) -> &str { &self.effect_name }
    pub fn card_y(&self) -> f32 { self.card_y }
    pub fn first_node(&self) -> usize { self.first_node }
    pub fn node_count(&self) -> usize { self.node_count }
    pub fn is_dragging(&self) -> bool {
        self.drag.is_dragging()
    }

    pub fn is_selected(&self) -> bool { self.is_selected }
    pub fn set_selected(&mut self, selected: bool) { self.is_selected = selected; }

    /// Unity EffectCardBitmapPanel.SetSelected (lines 244-254)
    /// Updates border color directly on the tree without a full rebuild.
    pub fn update_selection_visual(&mut self, tree: &mut UITree, selected: bool) {
        if selected == self.is_selected { return; }
        self.is_selected = selected;
        if self.border_id >= 0 {
            let color = if selected { color::SELECTED_BORDER } else { color::CARD_BORDER_C32 };
            tree.set_style(self.border_id as u32, UIStyle {
                bg_color: color,
                corner_radius: CORNER_RADIUS,
                ..UIStyle::default()
            });
        }
    }
    pub fn set_collapsed(&mut self, collapsed: bool) { self.is_collapsed = collapsed; }
    pub fn set_enabled(&mut self, enabled: bool) { self.enabled = enabled; }
    pub fn state_mut(&mut self) -> &mut EffectCardState { &mut self.state }

    pub fn compute_height(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed && !self.param_info.is_empty() {
            for i in 0..self.param_info.len() {
                h += ROW_HEIGHT + ROW_SPACING;
                if self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
                    h += DRIVER_CONFIG_HEIGHT;
                }
                if self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
                    h += ENV_CONFIG_HEIGHT;
                }
            }
        }
        h + CARD_BOTTOM_MARGIN
    }

    pub fn drag_handle_id(&self) -> i32 { self.drag_icon_id }

    /// Unity EffectCardBitmapPanel.IsDragHandle (line 228)
    pub fn is_drag_handle(&self, node_id: u32) -> bool {
        self.drag_icon_id >= 0 && node_id == self.drag_icon_id as u32
    }

    /// Unity EffectCardBitmapPanel.SetDragDimmed (lines 231-241)
    pub fn set_drag_dimmed(&self, tree: &mut UITree, dim: bool) {
        if self.border_id >= 0 {
            let color = if dim {
                Color32::new(46, 46, 49, 100) // Unity: dimmed border
            } else if self.is_selected {
                color::SELECTED_BORDER
            } else {
                color::CARD_BORDER_C32
            };
            tree.set_style(self.border_id as u32, UIStyle {
                bg_color: color,
                corner_radius: CORNER_RADIUS,
                ..UIStyle::default()
            });
        }
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.card_y = rect.y;
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);

        let effect_name = self.effect_name.clone();

        // Border — interactive so clicks on card edge also select
        let border_color = if self.is_selected { color::SELECTED_BORDER } else { color::CARD_BORDER_C32 };
        self.border_id = tree.add_panel(
            -1, rect.x, rect.y, rect.width, self.compute_height() - CARD_BOTTOM_MARGIN,
            UIStyle {
                bg_color: border_color,
                corner_radius: CORNER_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.border_id as u32, UIFlags::INTERACTIVE);

        // Inner background — interactive so clicks anywhere on card body select the card
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
        tree.set_flag(self.inner_bg_id as u32, UIFlags::INTERACTIVE);

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
        // Header background — interactive so clicks anywhere on header select the card
        self.header_bg_id = tree.add_panel(
            parent, x, y, w, HEADER_HEIGHT,
            UIStyle {
                bg_color: color::DRAG_HANDLE_BG_C32,
                corner_radius: CORNER_RADIUS - BORDER_W,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.header_bg_id as u32, UIFlags::INTERACTIVE);

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

        // ENV badge — visibility synced from state.has_env via sync_badges()
        let show_env = self.state.has_env;
        self.env_badge_bg_id = tree.add_panel(
            self.header_bg_id, env_x, badge_y, BADGE_W, BADGE_H,
            UIStyle {
                bg_color: color::ENVELOPE_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        self.env_badge_text_id = tree.add_label(
            self.env_badge_bg_id, env_x, badge_y, BADGE_W, BADGE_H,
            "ENV",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: 7,
                font_weight: FontWeight::Bold,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.env_badge_bg_id as u32, show_env);
        tree.set_visible(self.env_badge_text_id as u32, show_env);

        // DRV badge — visibility synced from state.has_drv via sync_badges()
        let show_drv = self.state.has_drv;
        self.drv_badge_bg_id = tree.add_panel(
            self.header_bg_id, drv_x, badge_y, BADGE_W, BADGE_H,
            UIStyle {
                bg_color: color::DRIVER_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        self.drv_badge_text_id = tree.add_label(
            self.drv_badge_bg_id, drv_x, badge_y, BADGE_W, BADGE_H,
            "DRV",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: 7,
                font_weight: FontWeight::Bold,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.drv_badge_bg_id as u32, show_drv);
        tree.set_visible(self.drv_badge_text_id as u32, show_drv);
        self.cached_has_env = show_env;
        self.cached_has_drv = show_drv;
        self.cached_enabled = self.enabled;

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
            let val_text = format_param_value(info.default, info.whole_numbers, info.value_labels.as_deref());

            // Param slider
            let slider_rect = Rect::new(x + PADDING, cy, slider_w, ROW_HEIGHT);
            self.slider_ids[i] = Some(BitmapSlider::build(
                tree, parent, slider_rect,
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

            // Envelope target (if envelope expanded)
            if self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
                if let Some(ref slider) = self.slider_ids[i] {
                    self.target_ids[i] = Some(build_envelope_target(
                        tree, slider.track as i32, slider.track_rect, &self.state.mod_state, i,
                    ));
                }
            }

            // D/E buttons (right side of row)
            let btn_x = x + PADDING + slider_w + DE_BUTTON_GAP;
            let btn_y = cy + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;

            if self.supports_envelopes {
                let env_active = self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false);
                self.envelope_btn_ids[i] = tree.add_button(
                    parent, btn_x, btn_y, DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                    de_btn_style(env_active, color::ENVELOPE_ACTIVE_C32),
                    "E",
                ) as i32;
            }

            let drv_active = self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false);
            let drv_btn_x = btn_x + if self.supports_envelopes { DE_BUTTON_SIZE + DE_BUTTON_GAP } else { 0.0 };
            self.driver_btn_ids[i] = tree.add_button(
                parent, drv_btn_x, btn_y, DE_BUTTON_SIZE, DE_BUTTON_SIZE,
                de_btn_style(drv_active, color::DRIVER_ACTIVE_C32),
                "\u{2192}", // →
            ) as i32;

            cy += ROW_HEIGHT + ROW_SPACING;

            // Envelope config drawer
            if self.state.mod_state.envelope_expanded.get(i).copied().unwrap_or(false) {
                self.envelope_config_ids[i] = Some(build_envelope_config(tree, parent, x + PADDING, cy, slider_w, &self.state.mod_state, i));
                cy += ENV_CONFIG_HEIGHT;
            }

            // Driver config drawer
            if self.state.mod_state.driver_expanded.get(i).copied().unwrap_or(false) {
                self.driver_config_ids[i] = Some(build_driver_config(tree, parent, x + PADDING, cy, slider_w, &self.state.mod_state, i, CONFIG_BTN_FONT_SIZE));
                cy += DRIVER_CONFIG_HEIGHT;
            }
        }
    }


    // ── Sync methods ─────────────────────────────────────────────

    /// Push param values from the engine. Updates sliders on change.
    /// Unity: EffectCardBitmapPanel.SyncValues — dirty-checks enabled, badges,
    /// and per-param values. Only updates tree when values actually changed.
    pub fn sync_values(&mut self, tree: &mut UITree, values: &[f32]) {
        // Toggle state dirty-check (Unity: state.enabled != cachedEnabled)
        if self.enabled != self.cached_enabled {
            self.cached_enabled = self.enabled;
            tree.set_style(self.toggle_btn_id as u32, toggle_btn_style(self.enabled));
            tree.set_text(self.toggle_btn_id as u32, if self.enabled { "ON" } else { "OFF" });
        }

        // Badge visibility dirty-check (Unity: ApplyModulationVisuals)
        if self.state.has_env != self.cached_has_env || self.state.has_drv != self.cached_has_drv {
            self.cached_has_env = self.state.has_env;
            self.cached_has_drv = self.state.has_drv;
            tree.set_visible(self.env_badge_bg_id as u32, self.cached_has_env);
            tree.set_visible(self.env_badge_text_id as u32, self.cached_has_env);
            tree.set_visible(self.drv_badge_bg_id as u32, self.cached_has_drv);
            tree.set_visible(self.drv_badge_text_id as u32, self.cached_has_drv);
        }

        // Skip slider sync if collapsed (Unity: if (state.collapsed) return)
        if self.is_collapsed { return; }

        // Per-param slider values (dirty-check via param_cache)
        for (i, &val) in values.iter().enumerate().take(self.param_info.len()) {
            if val != self.param_cache[i] || self.param_cache[i].is_nan() {
                self.param_cache[i] = val;
                if let Some(ref ids) = self.slider_ids[i] {
                    let info = &self.param_info[i];
                    let norm = BitmapSlider::value_to_normalized(val, info.min, info.max);
                    let text = format_param_value(val, info.whole_numbers, info.value_labels.as_deref());
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

    pub fn sync_enabled(&mut self, _tree: &mut UITree, enabled: bool) {
        // Just update the field — actual tree update happens in sync_values() dirty-check.
        // Unity: SyncValues() handles enabled state via dirty-checking.
        self.enabled = enabled;
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
        if let Some((pi, result)) = check_driver_config_click(id, &self.driver_config_ids) {
            let action = match result {
                DriverClickResult::BeatDiv(j) => DriverConfigAction::BeatDiv(j),
                DriverClickResult::Dot => DriverConfigAction::Dot,
                DriverClickResult::Triplet => DriverConfigAction::Triplet,
                DriverClickResult::Wave(j) => DriverConfigAction::Wave(j),
                DriverClickResult::Reverse => DriverConfigAction::Reverse,
            };
            return vec![PanelAction::EffectDriverConfig(ei, pi, action)];
        }

        // Card selection — any click on card background, border, or header triggers selection
        if id == self.border_id || id == self.header_bg_id || id == self.inner_bg_id
            || id == self.drag_icon_id || id == self.name_label_id
        {
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
                    self.drag.dragging_target_param = pi as i32;
                    return vec![PanelAction::EffectTargetSnapshot(ei, pi)];
                }
            }
        }

        // Check trim bars
        for (pi, trim) in self.trim_ids.iter().enumerate() {
            if let Some(ref t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = true;
                    return vec![PanelAction::EffectTrimSnapshot(ei, pi)];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = false;
                    return vec![PanelAction::EffectTrimSnapshot(ei, pi)];
                }
            }
        }

        // Check ADSR slider tracks
        for (pi, env_cfg) in self.envelope_config_ids.iter().enumerate() {
            if let Some(ref c) = env_cfg {
                if node_id == c.attack_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 0;
                    let norm = BitmapSlider::x_to_normalized(c.attack_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamSnapshot(ei, pi), PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Attack, norm * ENV_ADR_MAX)];
                }
                if node_id == c.decay_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 1;
                    let norm = BitmapSlider::x_to_normalized(c.decay_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamSnapshot(ei, pi), PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Decay, norm * ENV_ADR_MAX)];
                }
                if node_id == c.sustain_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 2;
                    let norm = BitmapSlider::x_to_normalized(c.sustain_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamSnapshot(ei, pi), PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Sustain, norm * ENV_S_MAX)];
                }
                if node_id == c.release_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 3;
                    let norm = BitmapSlider::x_to_normalized(c.release_slider.track_rect, pos.x);
                    return vec![PanelAction::EffectEnvParamSnapshot(ei, pi), PanelAction::EffectEnvParamChanged(ei, pi, EnvelopeParam::Release, norm * ENV_ADR_MAX)];
                }
            }
        }

        // Check param slider tracks.
        // When a driver is active, check if click is near a trim handle first —
        // the 4px trim bars are hard to hit precisely, so we use a proximity zone.
        // Unity: same 4px bars but event system delivers to correct child; we
        // use a wider hit zone (8px each side) for robustness.
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let Some(ref ids) = slider {
                if node_id == ids.track || {
                    // Also accept clicks on trim bar / fill / target nodes that are children of this track
                    self.trim_ids.get(pi).and_then(|t| t.as_ref()).map_or(false, |t|
                        node_id as i32 == t.fill_id || node_id as i32 == t.min_bar_id || node_id as i32 == t.max_bar_id
                    ) || self.target_ids.get(pi).and_then(|t| t.as_ref()).map_or(false, |t|
                        node_id as i32 == t.target_bar_id
                    )
                } {
                    // If driver is expanded, check proximity to trim handles before falling through to param drag
                    if self.state.mod_state.driver_expanded.get(pi).copied().unwrap_or(false) {
                        if let Some(ref trim) = self.trim_ids.get(pi).and_then(|t| t.as_ref()) {
                            let usable = ids.track_rect.width - OVERLAY_INSET * 2.0;
                            let base_x = ids.track_rect.x + OVERLAY_INSET;
                            let tmin = self.state.mod_state.trim_min.get(pi).copied().unwrap_or(0.0);
                            let tmax = self.state.mod_state.trim_max.get(pi).copied().unwrap_or(1.0);
                            let min_center = base_x + tmin * usable;
                            let max_center = base_x + tmax * usable;
                            let hit_zone = 8.0; // px proximity zone for trim handles

                            let dist_min = (pos.x - min_center).abs();
                            let dist_max = (pos.x - max_center).abs();

                            if dist_min < hit_zone && dist_min <= dist_max {
                                self.drag.dragging_trim_param = pi as i32;
                                self.drag.dragging_trim_is_min = true;
                                let _ = trim;
                                return vec![PanelAction::EffectTrimSnapshot(ei, pi)];
                            }
                            if dist_max < hit_zone {
                                self.drag.dragging_trim_param = pi as i32;
                                self.drag.dragging_trim_is_min = false;
                                return vec![PanelAction::EffectTrimSnapshot(ei, pi)];
                            }
                        }
                    }

                    // If envelope is expanded, check proximity to target bar before falling through
                    if self.state.mod_state.envelope_expanded.get(pi).copied().unwrap_or(false) {
                        let usable = ids.track_rect.width - OVERLAY_INSET * 2.0;
                        let base_x = ids.track_rect.x + OVERLAY_INSET;
                        let tgt = self.state.mod_state.target_norm.get(pi).copied().unwrap_or(1.0);
                        let target_center = base_x + tgt * usable;
                        let hit_zone = 8.0;

                        if (pos.x - target_center).abs() < hit_zone {
                            self.drag.dragging_target_param = pi as i32;
                            return vec![PanelAction::EffectTargetSnapshot(ei, pi)];
                        }
                    }

                    // No trim/target nearby — normal param slider drag
                    self.drag.dragging_param = pi as i32;
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

        // Target bar drag — update state, reposition bar node, dispatch action
        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            if let Some(ref slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                if let Some(ref mut state) = self.state.mod_state.target_norm.get_mut(pi) {
                    **state = norm;
                }

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

                return vec![PanelAction::EffectTargetChanged(ei, pi, norm)];
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

                return vec![PanelAction::EffectTrimChanged(ei, pi, new_min, new_max)];
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
                return vec![PanelAction::EffectEnvParamChanged(ei, pi, param, val)];
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
                return vec![PanelAction::EffectParamChanged(ei, pi, val)];
            }
        }

        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            self.drag.dragging_target_param = -1;
            return vec![PanelAction::EffectTargetCommit(ei, pi)];
        }
        if self.drag.dragging_trim_param >= 0 {
            let pi = self.drag.dragging_trim_param as usize;
            self.drag.dragging_trim_param = -1;
            return vec![PanelAction::EffectTrimCommit(ei, pi)];
        }
        if self.drag.dragging_env_param >= 0 {
            let pi = self.drag.dragging_env_param as usize;
            self.drag.dragging_env_param = -1;
            return vec![PanelAction::EffectEnvParamCommit(ei, pi)];
        }
        if self.drag.dragging_param >= 0 {
            let pi = self.drag.dragging_param as usize;
            self.drag.dragging_param = -1;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    fn test_config() -> EffectCardConfig {
        let n = 2;
        EffectCardConfig {
            effect_index: 0,
            name: "Blur".into(),
            enabled: true,
            collapsed: false,
            supports_envelopes: true,
            params: vec![
                EffectParamInfo { name: "Radius".into(), min: 0.0, max: 100.0, default: 10.0, whole_numbers: true, value_labels: None },
                EffectParamInfo { name: "Strength".into(), min: 0.0, max: 1.0, default: 0.5, whole_numbers: false, value_labels: None },
            ],
            has_drv: false,
            has_env: false,
            driver_active: vec![false; n],
            envelope_active: vec![false; n],
            trim_min: vec![0.0; n],
            trim_max: vec![1.0; n],
            target_norm: vec![1.0; n],
            env_attack: vec![0.0; n],
            env_decay: vec![0.0; n],
            env_sustain: vec![0.0; n],
            env_release: vec![0.0; n],
            driver_beat_div_idx: vec![-1; n],
            driver_waveform_idx: vec![-1; n],
            driver_reversed: vec![false; n],
            driver_dotted: vec![false; n],
            driver_triplet: vec![false; n],
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
        panel.state.mod_state.driver_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.driver_config_ids[0].is_some());
        assert!(panel.trim_ids[0].is_some());
    }

    #[test]
    fn effect_card_with_envelope_expanded() {
        let mut tree = UITree::new();
        let mut panel = EffectCardPanel::new();
        panel.configure(&test_config());
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.envelope_config_ids[0].is_some());
        assert!(panel.target_ids[0].is_some());
    }
}
