//! Unified parameter-card data contract shared by the effect and generator
//! inspector cards.
//!
//! Effects and generators present the same instrument to the user — a card
//! with a header, a column of parameter rows (each a slider plus optional
//! driver / envelope / Ableton modulation drawers), and a few kind-specific
//! affordances. Historically each side carried its own `…ParamInfo` /
//! `…Config` structs that were field-for-field near-duplicates. This module
//! is the single source of truth for that data contract; both panels consume
//! it.
//!
//! The small real differences between the two kinds live on these structs as
//! kind-tagged or optional fields (effect-only: `enabled`, badges,
//! `has_graph_mod`; generator-only: `string_params`, `is_toggle`,
//! `is_trigger`). Readers branch on [`ParamCardKind`] or ignore the field
//! that doesn't apply to them.

use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_slider_shared::*;
use super::{EnvelopeParam, PanelAction};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use manifold_core::{EffectId, LayerId};

/// Which kind of preset a card is displaying. Carries the small set of real
/// behavioral differences between the effect and generator inspector cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamCardKind {
    Effect,
    Generator,
}

/// Per-parameter configuration info provided by the app layer. One per slot
/// in the host's `param_values`, in declaration order (static prefix, then
/// user-exposed tail for effects).
#[derive(Debug, Clone)]
pub struct ParamInfo {
    /// Stable [`ParamId`](manifold_core::effects::ParamId) for this slot — for
    /// static-tier params the `&'static str` declared in the preset's
    /// `ParamSpec`; for user-tier (graph-editor-exposed) effect params the
    /// owned id from `EffectInstance.user_param_bindings[j].id`. Carried on
    /// the wire when a widget emits a [`PanelAction`](super::PanelAction) so
    /// the bridge never does a positional `pi → ParamId` lookup.
    pub param_id: manifold_core::effects::ParamId,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    /// Whether this slot is exposed as a slider on the card. `false` hides the
    /// slider widget while preserving slot-index semantics (drivers / Ableton
    /// mappings keep working — just no visible slider). Defaults to `true`.
    /// Effects have always carried this; generators gained it with the
    /// `ParamSlot` storage unification.
    pub exposed: bool,
    /// Generator toggle param — renders as a boolean ON/OFF button row instead
    /// of a slider. Always `false` for effect params today.
    pub is_toggle: bool,
    /// Momentary "fire once" button — renders as a `▶` button row (no slider).
    /// Click increments the underlying monotonic counter by one; consumed via
    /// the same `ParamConvert::Trigger` plumbing as wired trigger inputs.
    pub is_trigger: bool,
    /// Named value labels for discrete params (e.g. `["Horiz","Vert","Both"]`).
    /// When present the slider shows the label instead of a numeric value.
    pub value_labels: Option<Vec<String>>,
    /// OSC address for this parameter (e.g. `/master/bloom/amount`). When
    /// present, clicking the param label copies this address to the clipboard.
    pub osc_address: Option<String>,
    /// When set, an Ableton mapping sub-section is shown below the slider.
    pub ableton_display: Option<AbletonMappingDisplay>,
    /// Ableton trim range `(range_min, range_max)`. When present, trim handles
    /// are shown on the slider track.
    pub ableton_range: Option<(f32, f32)>,
}

/// A generator string parameter — rendered as a clickable text-field row
/// below the slider rows. Generator-only; effects carry an empty list.
#[derive(Debug, Clone)]
pub struct ParamCardStringInfo {
    pub name: String,
    pub key: String,
    pub value: String,
    /// If true, clicking this param opens a dropdown instead of text input.
    pub use_dropdown: bool,
}

/// Configuration for building / refreshing one parameter card.
///
/// The union of what the effect and generator cards need. Effect-only fields
/// (`effect_index`, `effect_id`, `enabled`, `supports_envelopes`, the badge
/// aggregates, `has_graph_mod`) carry defaults for generators; the
/// generator-only `string_params` / `layer_id` carry empty / `None` for
/// effects. The `…_active` / `trim_*` / `env_*` / `driver_*` vectors are the
/// shared per-param modulation state both kinds drive identically.
#[derive(Debug, Clone)]
pub struct ParamCardConfig {
    pub kind: ParamCardKind,
    /// Display name — the effect name or the generator type name.
    pub name: String,
    pub params: Vec<ParamInfo>,
    /// Generator string params (clickable text-field rows). Empty for effects.
    pub string_params: Vec<ParamCardStringInfo>,
    pub collapsed: bool,

    // ── Effect-only identity + flags (defaults for generators) ──
    pub effect_index: usize,
    pub effect_id: EffectId,
    pub enabled: bool,
    pub supports_envelopes: bool,
    /// Aggregate: any param has an active driver (DRV badge).
    pub has_drv: bool,
    /// Aggregate: any param has an active envelope (ENV badge).
    pub has_env: bool,
    /// Aggregate: any param has an Ableton mapping (ABL badge).
    pub has_abl: bool,
    /// The effect instance carries a per-card graph override
    /// (`EffectInstance.graph.is_some()`) — drives the pink "MOD" badge +
    /// header tint.
    pub has_graph_mod: bool,

    // ── Generator-only identity ──
    pub layer_id: Option<LayerId>,

    // ── Shared per-param modulation state ──
    /// Per-param: driver exists and is enabled.
    pub driver_active: Vec<bool>,
    /// Per-param: envelope exists and is enabled.
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
    /// Per-param envelope mode (ADSR or Random).
    pub env_mode: Vec<EnvelopeMode>,
    /// Per-param random_jump flag.
    pub env_random_jump: Vec<bool>,
    /// Per-param envelope range min (normalized). Defaults to 0.0.
    pub env_range_min: Vec<f32>,
    /// Per-param envelope range max (normalized). Defaults to 1.0.
    pub env_range_max: Vec<f32>,
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

// ── Layout constants ─────────────────────────────────────────────
//
// Shared between both card kinds. The shell furniture each kind draws on top
// (effect: drag-handle + ABL/ENV/DRV/MOD badges + ON/OFF toggle; generator:
// Change button) carries its own kind-specific widths.

const HEADER_HEIGHT: f32 = 27.5;
const BORDER_W: f32 = 1.0;
const CORNER_RADIUS: f32 = 4.0;
const CARD_BOTTOM_MARGIN: f32 = 6.0;
const CHEVRON_W: f32 = 18.0;
const COG_W: f32 = 18.0;

// Effect shell furniture.
const DRAG_HANDLE_W: f32 = 18.0;
const TOGGLE_W: f32 = 30.0;
const BADGE_W: f32 = 36.0;
const BADGE_H: f32 = 14.0;
const BADGE_RADIUS: f32 = 7.0;
const CONFIG_BTN_FONT_SIZE: u16 = color::FONT_CAPTION;

// Generator shell furniture.
const TOGGLE_BTN_W: f32 = 40.0;
const TOGGLE_BTN_H: f32 = 16.0;
const CHANGE_BTN_W: f32 = 60.0;
const CHANGE_BTN_H: f32 = 16.0;

// ── Internal node ID structs ─────────────────────────────────────

/// Generator toggle/trigger row node IDs (button + its label).
struct ToggleParamIds {
    _label_id: i32,
    button_id: i32,
}

// ── ParamCardState ───────────────────────────────────────────────

/// Presenter-owned visual state for one parameter card — the single source of
/// truth for all data-derived visuals (badges + per-param modulation). Unifies
/// the former `EffectCardState` / `GenParamState`. The badge aggregates only
/// drive the effect-card header chips; generators leave them `false`.
pub struct ParamCardState {
    /// Aggregate: any param has an active driver (DRV badge).
    pub has_drv: bool,
    /// Aggregate: any param has an active envelope (ENV badge).
    pub has_env: bool,
    /// Aggregate: any param has an Ableton mapping (ABL badge).
    pub has_abl: bool,
    /// The card's graph diverges from the catalog default (MOD badge + tint).
    pub has_graph_mod: bool,
    /// Shared per-param modulation state (driver/envelope expansion, trim,
    /// target, ADSR, driver config).
    pub mod_state: ParamModState,
}

impl ParamCardState {
    pub fn new(param_count: usize) -> Self {
        Self {
            has_drv: false,
            has_env: false,
            has_abl: false,
            has_graph_mod: false,
            mod_state: ParamModState::allocate(param_count),
        }
    }
}

// ── ParamCardPanel ───────────────────────────────────────────────

/// The unified inspector parameter card. One struct presents both effect cards
/// and generator cards; [`kind`](ParamCardKind) selects the shell furniture
/// (effect: drag-handle + badges + ON/OFF toggle + hierarchical parenting;
/// generator: Change button + toggle/trigger/string rows + flat parenting)
/// while the per-parameter row core — slider + trim/target/range handles + D/E
/// buttons + driver/envelope/Ableton drawers — is shared verbatim via
/// [`build_param_row`] / [`match_param_row_click`]. The drag-move and
/// drag-end dispatch are shared too, branching on `kind` only at the
/// [`PanelAction`] emission points.
pub struct ParamCardPanel {
    kind: ParamCardKind,

    // ── Identity ──
    /// Effect chain position (effect kind).
    effect_index: usize,
    /// Effect instance id (effect kind).
    effect_id: EffectId,
    /// Display name — effect name or generator type name.
    name: String,
    /// Owning layer (generator kind).
    layer_id: Option<LayerId>,

    // ── Configuration ──
    enabled: bool,
    is_collapsed: bool,
    is_selected: bool,
    supports_envelopes: bool,
    param_info: Vec<ParamInfo>,
    string_param_info: Vec<ParamCardStringInfo>,

    // ── State ──
    state: ParamCardState,

    // ── Node IDs — card shell (shared) ──
    border_id: i32,
    inner_bg_id: i32,
    header_bg_id: i32,
    name_label_id: i32,
    chevron_btn_id: i32,
    cog_btn_id: i32,

    // ── Node IDs — effect shell ──
    drag_icon_id: i32,
    toggle_btn_id: i32,
    abl_badge_bg_id: i32,
    abl_badge_text_id: i32,
    env_badge_bg_id: i32,
    env_badge_text_id: i32,
    drv_badge_bg_id: i32,
    drv_badge_text_id: i32,
    mod_badge_bg_id: i32,
    mod_badge_text_id: i32,

    // ── Node IDs — generator shell ──
    change_btn_id: i32,

    // ── Dirty-check cache (effect badges + enabled) ──
    cached_enabled: bool,
    cached_has_env: bool,
    cached_has_drv: bool,
    cached_has_abl: bool,
    cached_has_graph_mod: bool,

    // ── Node IDs — per-param (shared) ──
    slider_ids: Vec<Option<SliderNodeIds>>,
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

    // ── Node IDs — per-param (generator) ──
    toggle_ids: Vec<Option<ToggleParamIds>>,
    string_param_btn_ids: Vec<i32>,

    // Per-param OSC addresses (for click-to-copy). Indexed by param index.
    osc_addresses: Vec<Option<String>>,

    copied_flash: CopyToClipboardLabelState,

    // Drag state
    drag: ParamDragState,

    // Caches (NaN = needs sync)
    param_cache: Vec<f32>,
    toggle_cache: Vec<bool>,
    label_cache: Vec<Option<String>>,

    // Node range
    first_node: usize,
    node_count: usize,

    // Card position (for effect drag-reorder hit testing)
    card_y: f32,
}

impl ParamCardPanel {
    pub fn new() -> Self {
        Self {
            kind: ParamCardKind::Effect,
            effect_index: 0,
            effect_id: EffectId::default(),
            name: String::new(),
            layer_id: None,
            enabled: true,
            is_collapsed: false,
            is_selected: false,
            supports_envelopes: true,
            param_info: Vec::new(),
            string_param_info: Vec::new(),
            state: ParamCardState::new(0),
            border_id: -1,
            inner_bg_id: -1,
            header_bg_id: -1,
            name_label_id: -1,
            chevron_btn_id: -1,
            cog_btn_id: -1,
            drag_icon_id: -1,
            toggle_btn_id: -1,
            abl_badge_bg_id: -1,
            abl_badge_text_id: -1,
            env_badge_bg_id: -1,
            env_badge_text_id: -1,
            drv_badge_bg_id: -1,
            drv_badge_text_id: -1,
            mod_badge_bg_id: -1,
            mod_badge_text_id: -1,
            change_btn_id: -1,
            cached_enabled: true,
            cached_has_env: false,
            cached_has_drv: false,
            cached_has_abl: false,
            cached_has_graph_mod: false,
            slider_ids: Vec::new(),
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
            toggle_ids: Vec::new(),
            string_param_btn_ids: Vec::new(),
            osc_addresses: Vec::new(),
            copied_flash: CopyToClipboardLabelState::default(),
            drag: ParamDragState::new(),
            param_cache: Vec::new(),
            toggle_cache: Vec::new(),
            label_cache: Vec::new(),
            first_node: 0,
            node_count: 0,
            card_y: 0.0,
        }
    }

    /// Configure from card metadata. Call before [`build`](Self::build).
    ///
    /// Sets `kind` from the config and populates every data-derived field for
    /// both shells (effect identity/badges + generator string params), so the
    /// same call serves either kind. The owning `layer_id` is NOT touched here
    /// — it is set independently via [`set_layer_id`](Self::set_layer_id)
    /// before configure (the generator config doesn't carry it).
    pub fn configure(&mut self, config: &ParamCardConfig) {
        self.kind = config.kind;
        self.effect_index = config.effect_index;
        self.effect_id = config.effect_id.clone();
        self.name = config.name.clone();
        self.enabled = config.enabled;
        self.is_collapsed = config.collapsed;
        self.supports_envelopes = config.supports_envelopes;
        self.param_info = config.params.clone();
        self.string_param_info = config.string_params.clone();

        let n = config.params.len();
        self.state = ParamCardState::new(n);
        self.state.has_drv = config.has_drv;
        self.state.has_env = config.has_env;
        self.state.has_abl = config.has_abl;
        self.state.has_graph_mod = config.has_graph_mod;
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
        self.osc_addresses = config
            .params
            .iter()
            .map(|p| p.osc_address.clone())
            .collect();
        self.copied_flash.clear();
        self.slider_ids = vec![None; n];
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
        self.toggle_ids = Vec::new();
        self.toggle_ids.resize_with(n, || None);
        self.string_param_btn_ids = vec![-1; config.string_params.len()];
        self.param_cache = vec![f32::NAN; n];
        self.toggle_cache = vec![false; n];
        self.label_cache = vec![None; n];
    }

    // ── Accessors ─────────────────────────────────────────────────

    pub fn effect_index(&self) -> usize {
        self.effect_index
    }
    pub fn effect_id(&self) -> &EffectId {
        &self.effect_id
    }
    pub fn effect_name(&self) -> &str {
        &self.name
    }
    pub fn card_y(&self) -> f32 {
        self.card_y
    }
    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    pub fn is_dragging(&self) -> bool {
        self.drag.is_dragging()
    }
    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }
    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.is_collapsed = collapsed;
    }
    pub fn state_mut(&mut self) -> &mut ParamCardState {
        &mut self.state
    }
    pub fn set_layer_id(&mut self, id: Option<LayerId>) {
        self.layer_id = id;
    }

    /// Border color for the card's current kind + state.
    fn base_border_color(&self) -> Color32 {
        if self.is_selected {
            color::SELECTED_BORDER
        } else {
            match self.kind {
                ParamCardKind::Effect => color::CARD_BORDER_C32,
                ParamCardKind::Generator => color::GEN_CARD_BORDER_C32,
            }
        }
    }

    /// Update the border color without a full rebuild (selection highlight).
    pub fn update_selection_visual(&mut self, tree: &mut UITree, selected: bool) {
        if selected == self.is_selected {
            return;
        }
        self.is_selected = selected;
        if self.border_id >= 0 {
            tree.set_style(
                self.border_id as u32,
                UIStyle {
                    bg_color: self.base_border_color(),
                    corner_radius: CORNER_RADIUS,
                    ..UIStyle::default()
                },
            );
        }
    }

    /// Returns true if the param identified by `param_id` has an Ableton
    /// mapping. Keyed by stable id so user-exposed inner-graph params resolve
    /// transparently (no positional `pi` lookup that would miss the user tail).
    pub fn param_has_ableton_mapping(&self, param_id: &str) -> bool {
        self.param_info
            .iter()
            .find(|p| p.param_id == param_id)
            .is_some_and(|p| p.ableton_display.is_some())
    }

    /// Whether `node_id` is this card's drag handle (effect kind only).
    pub fn is_drag_handle(&self, node_id: u32) -> bool {
        self.drag_icon_id >= 0 && node_id == self.drag_icon_id as u32
    }

    /// Dim/undim the card border during a reorder drag (effect kind).
    pub fn set_drag_dimmed(&self, tree: &mut UITree, dim: bool) {
        if self.border_id >= 0 {
            let bg_color = if dim {
                Color32::new(46, 46, 49, 100) // dimmed border
            } else {
                self.base_border_color()
            };
            tree.set_style(
                self.border_id as u32,
                UIStyle {
                    bg_color,
                    corner_radius: CORNER_RADIUS,
                    ..UIStyle::default()
                },
            );
        }
    }

    /// Get string param info for text input anchoring (generator kind).
    pub fn string_param(&self, index: usize) -> Option<&ParamCardStringInfo> {
        self.string_param_info.get(index)
    }

    /// Get the screen-space rect of a string param button for text input
    /// anchoring (generator kind).
    pub fn string_param_rect(&self, tree: &UITree, index: usize) -> Option<Rect> {
        self.string_param_btn_ids
            .get(index)
            .filter(|&&id| id >= 0)
            .map(|&id| tree.get_bounds(id as u32))
    }

    // ── compute_height ────────────────────────────────────────────

    pub fn compute_height(&self) -> f32 {
        match self.kind {
            ParamCardKind::Effect => self.compute_height_effect(),
            ParamCardKind::Generator => self.compute_height_generator(),
        }
    }

    fn compute_height_effect(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed && !self.param_info.is_empty() {
            for i in 0..self.param_info.len() {
                // Hidden params consume zero vertical space.
                if !self.param_info[i].exposed {
                    continue;
                }
                h += ROW_HEIGHT + ROW_SPACING;
                h += self.row_drawer_height(i);
            }
        }
        h + CARD_BOTTOM_MARGIN
    }

    fn compute_height_generator(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed {
            for (i, info) in self.param_info.iter().enumerate() {
                if info.is_toggle || info.is_trigger {
                    h += ROW_HEIGHT + ROW_SPACING;
                } else {
                    h += ROW_HEIGHT + ROW_SPACING;
                    h += self.row_drawer_height(i);
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

    /// Height contributed by the expanded driver/envelope/Ableton drawers for
    /// one slider param. Shared by both kinds' height computations.
    fn row_drawer_height(&self, i: usize) -> f32 {
        let mut h = 0.0;
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
        if self.param_info[i].ableton_display.is_some() {
            h += ABL_CONFIG_HEIGHT;
        }
        h
    }

    // ── Build ─────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        match self.kind {
            ParamCardKind::Effect => self.build_effect(tree, rect),
            ParamCardKind::Generator => self.build_generator(tree, rect),
        }
    }

    fn build_effect(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.card_y = rect.y;
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.label_cache.iter_mut().for_each(|v| *v = None);

        let effect_name = self.name.clone();

        // Border — interactive so clicks on card edge also select
        let border_color = if self.is_selected {
            color::SELECTED_BORDER
        } else {
            color::CARD_BORDER_C32
        };
        self.border_id = tree.add_panel(
            -1,
            rect.x,
            rect.y,
            rect.width,
            self.compute_height() - CARD_BOTTOM_MARGIN,
            UIStyle {
                bg_color: border_color,
                corner_radius: CORNER_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.border_id as u32, UIFlags::INTERACTIVE);

        // Inner background — interactive so clicks anywhere on card body select the card
        let inner = Rect::new(
            rect.x + BORDER_W,
            rect.y + BORDER_W,
            rect.width - BORDER_W * 2.0,
            self.compute_height() - CARD_BOTTOM_MARGIN - BORDER_W * 2.0,
        );
        self.inner_bg_id = tree.add_panel(
            self.border_id,
            inner.x,
            inner.y,
            inner.width,
            inner.height,
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
        self.build_effect_header(tree, parent, inner.x, inner.y, inner_w, &effect_name);

        // Param sliders
        if !self.is_collapsed && !self.param_info.is_empty() {
            self.build_effect_sliders(tree, parent, inner.x, inner.y + HEADER_HEIGHT, inner_w);
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_effect_header(
        &mut self,
        tree: &mut UITree,
        parent: i32,
        x: f32,
        y: f32,
        w: f32,
        name: &str,
    ) {
        // Header background — interactive so clicks anywhere on header select the card.
        // Tint pink when the card carries a per-card graph override.
        let header_bg = if self.state.has_graph_mod {
            color::MOD_HEADER_BG_C32
        } else {
            color::DRAG_HANDLE_BG_C32
        };
        self.header_bg_id = tree.add_panel(
            parent,
            x,
            y,
            w,
            HEADER_HEIGHT,
            UIStyle {
                bg_color: header_bg,
                corner_radius: CORNER_RADIUS - BORDER_W,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.header_bg_id as u32, UIFlags::INTERACTIVE);

        // Layout (right-to-left for fixed elements). MOD badge sits
        // between the name label and the existing ABL/ENV/DRV chips.
        let cog_x = x + w - PADDING - COG_W;
        let chevron_x = cog_x - GAP - CHEVRON_W;
        let toggle_x = chevron_x - GAP - TOGGLE_W;
        let drv_x = toggle_x - GAP - BADGE_W;
        let env_x = drv_x - GAP - BADGE_W;
        let abl_x = env_x - GAP - BADGE_W;
        let mod_x = abl_x - GAP - BADGE_W;
        let name_x = x + PADDING + DRAG_HANDLE_W + GAP;
        let name_w = (mod_x - GAP - name_x).max(10.0);
        let elem_y = y + (HEADER_HEIGHT - 16.0) * 0.5;
        let badge_y = y + (HEADER_HEIGHT - BADGE_H) * 0.5;

        // Drag handle (hamburger icon drawn as 3 horizontal bars)
        let dh_x = x + PADDING;
        let dh_h = 16.0_f32;
        self.drag_icon_id = tree.add_button(
            self.header_bg_id,
            dh_x,
            elem_y,
            DRAG_HANDLE_W,
            dh_h,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::DRAG_HANDLE_HOVER_BG_C32,
                pressed_bg_color: color::DRAG_HANDLE_BG_C32,
                ..UIStyle::default()
            },
            "",
        ) as i32;
        {
            let bar_w: f32 = 10.0;
            let bar_h: f32 = 1.5;
            let bar_x = dh_x + (DRAG_HANDLE_W - bar_w) * 0.5;
            let bar_color = color::TEXT_DIMMED_C32;
            let bar_style = UIStyle {
                bg_color: bar_color,
                ..UIStyle::default()
            };
            for i in 0..3 {
                let bar_y = elem_y + 3.5 + i as f32 * 3.5;
                tree.add_panel(self.drag_icon_id, bar_x, bar_y, bar_w, bar_h, bar_style);
            }
        }

        // Name label
        self.name_label_id = tree.add_label(
            self.header_bg_id,
            name_x,
            elem_y,
            name_w,
            16.0,
            name,
            UIStyle {
                text_color: color::EFFECT_HEADER_NAME,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        // ABL badge — visibility synced from state.has_abl
        let show_abl = self.state.has_abl;
        self.abl_badge_bg_id = tree.add_panel(
            self.header_bg_id,
            abl_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::ABL_BADGE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        self.abl_badge_text_id = tree.add_label(
            self.abl_badge_bg_id,
            abl_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "ABL",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.abl_badge_bg_id as u32, show_abl);
        tree.set_visible(self.abl_badge_text_id as u32, show_abl);

        // ENV badge — visibility synced from state.has_env
        let show_env = self.state.has_env;
        self.env_badge_bg_id = tree.add_panel(
            self.header_bg_id,
            env_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::ENVELOPE_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        self.env_badge_text_id = tree.add_label(
            self.env_badge_bg_id,
            env_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "ENV",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.env_badge_bg_id as u32, show_env);
        tree.set_visible(self.env_badge_text_id as u32, show_env);

        // DRV badge — visibility synced from state.has_drv
        let show_drv = self.state.has_drv;
        self.drv_badge_bg_id = tree.add_panel(
            self.header_bg_id,
            drv_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::DRIVER_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        self.drv_badge_text_id = tree.add_label(
            self.drv_badge_bg_id,
            drv_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "DRV",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.drv_badge_bg_id as u32, show_drv);
        tree.set_visible(self.drv_badge_text_id as u32, show_drv);

        // MOD badge — pink chip indicating the card's graph topology
        // diverges from the catalog default.
        let show_mod = self.state.has_graph_mod;
        self.mod_badge_bg_id = tree.add_panel(
            self.header_bg_id,
            mod_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::MOD_BADGE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        self.mod_badge_text_id = tree.add_label(
            self.mod_badge_bg_id,
            mod_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "MOD",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_visible(self.mod_badge_bg_id as u32, show_mod);
        tree.set_visible(self.mod_badge_text_id as u32, show_mod);

        self.cached_has_env = show_env;
        self.cached_has_drv = show_drv;
        self.cached_has_abl = show_abl;
        self.cached_has_graph_mod = show_mod;
        self.cached_enabled = self.enabled;

        // Toggle button (ON/OFF)
        let toggle_style = toggle_btn_style(self.enabled);
        self.toggle_btn_id = tree.add_button(
            self.header_bg_id,
            toggle_x,
            elem_y,
            TOGGLE_W,
            16.0,
            toggle_style,
            if self.enabled { "ON" } else { "OFF" },
        ) as i32;

        // Chevron
        self.chevron_btn_id = tree.add_button(
            self.header_bg_id,
            chevron_x,
            elem_y,
            CHEVRON_W,
            16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.is_collapsed {
                "\u{25B6}"
            } else {
                "\u{25BC}"
            },
        ) as i32;

        // "Open in graph editor" affordance — three small dots in a triangle.
        self.cog_btn_id = tree.add_button(
            self.header_bg_id,
            cog_x,
            elem_y,
            COG_W,
            16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                ..UIStyle::default()
            },
            "",
        ) as i32;
        {
            let dot: f32 = 3.0;
            let dot_color = color::TEXT_DIMMED_C32;
            let dot_style = UIStyle {
                bg_color: dot_color,
                corner_radius: dot * 0.5,
                ..UIStyle::default()
            };
            let cx = cog_x + COG_W * 0.5;
            let cy = elem_y + 8.0;
            let v_offset = 3.5;
            let h_offset = 4.0;
            let positions = [
                (cx - dot * 0.5, cy - v_offset - dot * 0.5),
                (cx - h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
                (cx + h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
            ];
            for (px, py) in positions {
                tree.add_panel(self.cog_btn_id, px, py, dot, dot, dot_style);
            }
        }
    }

    fn build_effect_sliders(&mut self, tree: &mut UITree, parent: i32, x: f32, start_y: f32, w: f32) {
        let mut cy = start_y;
        let slider_w = w - PADDING * 2.0 - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 2.0;

        for i in 0..self.param_info.len() {
            // Hidden params: leave slider_ids[i] = None and skip widget
            // construction entirely. Slot-index semantics for any attached
            // driver/Ableton mapping/envelope are preserved.
            if !self.param_info[i].exposed {
                continue;
            }
            let info = self.param_info[i].clone();
            // Per-param slider + driver/envelope/Ableton drawers — the shared
            // core. Effects nest rows under `parent` (the inner-bg panel), use
            // the default slider palette + caption-size driver-config font, and
            // gate the `E` button on `supports_envelopes`.
            let row = build_param_row(
                tree,
                parent,
                x + PADDING,
                cy,
                slider_w,
                w - PADDING * 2.0,
                &info,
                &self.state.mod_state,
                i,
                &SliderColors::default_slider(),
                CONFIG_BTN_FONT_SIZE,
                self.supports_envelopes,
            );
            self.slider_ids[i] = row.slider;
            self.trim_ids[i] = row.trim;
            self.target_ids[i] = row.target;
            self.envelope_range_ids[i] = row.envelope_range;
            self.ableton_trim_ids[i] = row.ableton_trim;
            self.envelope_btn_ids[i] = row.envelope_btn;
            self.driver_btn_ids[i] = row.driver_btn;
            self.envelope_config_ids[i] = row.envelope_config;
            self.envelope_random_config_ids[i] = row.envelope_random_config;
            self.driver_config_ids[i] = row.driver_config;
            self.ableton_config_ids[i] = row.ableton_config;
            cy = row.new_cy;
        }
    }

    fn build_generator(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.card_y = rect.y;
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

        let gen_name = self.name.clone();

        // Header layout (right-to-left): [Name] ... [Change] [Cog] [Chevron]
        let chevron_x = inner_x + inner_w - CHEVRON_W;
        let cog_x = chevron_x - COG_W;
        let change_x = cog_x - CHANGE_BTN_W - GAP;
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
        self.chevron_btn_id = tree.add_button(
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

        // "Open in graph editor" affordance — three small dots in a triangle.
        let elem_y = inner_y;
        self.cog_btn_id = tree.add_button(
            -1,
            cog_x,
            elem_y,
            COG_W,
            HEADER_HEIGHT,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                ..UIStyle::default()
            },
            "",
        ) as i32;
        {
            let dot: f32 = 3.0;
            let dot_style = UIStyle {
                bg_color: color::TEXT_DIMMED_C32,
                corner_radius: dot * 0.5,
                ..UIStyle::default()
            };
            let cx = cog_x + COG_W * 0.5;
            let cy = elem_y + HEADER_HEIGHT * 0.5;
            let v_offset = 3.5;
            let h_offset = 4.0;
            let positions = [
                (cx - dot * 0.5, cy - v_offset - dot * 0.5),
                (cx - h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
                (cx + h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
            ];
            for (px, py) in positions {
                tree.add_panel(self.cog_btn_id, px, py, dot, dot, dot_style);
            }
        }

        // ── Params (if not collapsed) ──
        if !self.is_collapsed && !self.param_info.is_empty() {
            let content_w = inner_w - PADDING * 2.0;
            let cx = inner_x + PADDING;
            let mut cy = inner_y + HEADER_HEIGHT;
            let slider_w = content_w - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 2.0;

            for i in 0..self.param_info.len() {
                let info = self.param_info[i].clone();

                if info.is_toggle || info.is_trigger {
                    // Toggle / Trigger row — both share the button-row layout.
                    // ON/OFF for sticky toggles, ▶ for momentary fire-once
                    // triggers. Click handler dispatches differently (toggle vs
                    // fire) based on the is_trigger flag.
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
                    let (button_text, button_style) = if info.is_trigger {
                        // Trigger renders as a momentary button — always neutral.
                        ("▶", toggle_btn_style(false))
                    } else {
                        (if on { "ON" } else { "OFF" }, toggle_btn_style(on))
                    };
                    let button_id = tree.add_button(
                        -1,
                        cx + content_w - TOGGLE_BTN_W,
                        cy + (ROW_HEIGHT - TOGGLE_BTN_H) * 0.5,
                        TOGGLE_BTN_W,
                        TOGGLE_BTN_H,
                        button_style,
                        button_text,
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
                    // Slider row — shared per-param core. Generators parent rows
                    // flat to `-1`, use the gen-param slider palette, the
                    // body-size driver-config font, and always show the `E`
                    // button (generators always support envelopes).
                    let row = build_param_row(
                        tree,
                        -1,
                        cx,
                        cy,
                        slider_w,
                        content_w,
                        &info,
                        &self.state.mod_state,
                        i,
                        &SliderColors::gen_param(),
                        FONT_SIZE,
                        true,
                    );
                    self.slider_ids[i] = row.slider;
                    self.trim_ids[i] = row.trim;
                    self.target_ids[i] = row.target;
                    self.envelope_range_ids[i] = row.envelope_range;
                    self.ableton_trim_ids[i] = row.ableton_trim;
                    self.envelope_btn_ids[i] = row.envelope_btn;
                    self.driver_btn_ids[i] = row.driver_btn;
                    self.envelope_config_ids[i] = row.envelope_config;
                    self.envelope_random_config_ids[i] = row.envelope_random_config;
                    self.driver_config_ids[i] = row.driver_config;
                    self.ableton_config_ids[i] = row.ableton_config;
                    cy = row.new_cy;
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

    // ── Sync methods ──────────────────────────────────────────────

    pub fn sync_values(&mut self, tree: &mut UITree, values: &[manifold_core::effects::ParamSlot]) {
        match self.kind {
            ParamCardKind::Effect => self.sync_values_effect(tree, values),
            ParamCardKind::Generator => self.sync_values_generator(tree, values),
        }
    }

    fn sync_values_effect(
        &mut self,
        tree: &mut UITree,
        values: &[manifold_core::effects::ParamSlot],
    ) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| {
                self.slider_ids
                    .iter()
                    .enumerate()
                    .find_map(|(pi, s)| {
                        s.as_ref()
                            .filter(|ids| ids.label >= 0 && ids.label as u32 == label_id)
                            .and_then(|_| self.param_info.get(pi).map(|p| p.name.clone()))
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        // Toggle state dirty-check
        if self.enabled != self.cached_enabled {
            self.cached_enabled = self.enabled;
            tree.set_style(self.toggle_btn_id as u32, toggle_btn_style(self.enabled));
            tree.set_text(
                self.toggle_btn_id as u32,
                if self.enabled { "ON" } else { "OFF" },
            );
        }

        // Badge visibility dirty-check
        if self.state.has_env != self.cached_has_env
            || self.state.has_drv != self.cached_has_drv
            || self.state.has_abl != self.cached_has_abl
            || self.state.has_graph_mod != self.cached_has_graph_mod
        {
            self.cached_has_env = self.state.has_env;
            self.cached_has_drv = self.state.has_drv;
            self.cached_has_abl = self.state.has_abl;
            self.cached_has_graph_mod = self.state.has_graph_mod;
            tree.set_visible(self.abl_badge_bg_id as u32, self.cached_has_abl);
            tree.set_visible(self.abl_badge_text_id as u32, self.cached_has_abl);
            tree.set_visible(self.env_badge_bg_id as u32, self.cached_has_env);
            tree.set_visible(self.env_badge_text_id as u32, self.cached_has_env);
            tree.set_visible(self.drv_badge_bg_id as u32, self.cached_has_drv);
            tree.set_visible(self.drv_badge_text_id as u32, self.cached_has_drv);
            tree.set_visible(self.mod_badge_bg_id as u32, self.cached_has_graph_mod);
            tree.set_visible(self.mod_badge_text_id as u32, self.cached_has_graph_mod);
            // Re-tint the header background when the modified-state flips.
            let header_bg = if self.cached_has_graph_mod {
                color::MOD_HEADER_BG_C32
            } else {
                color::DRAG_HANDLE_BG_C32
            };
            tree.set_style(
                self.header_bg_id as u32,
                UIStyle {
                    bg_color: header_bg,
                    corner_radius: CORNER_RADIUS - BORDER_W,
                    ..UIStyle::default()
                },
            );
        }

        // Skip slider sync if collapsed
        if self.is_collapsed {
            return;
        }

        // Per-param slider values + label (dirty-check via param_cache / label_cache)
        for (i, slot) in values.iter().enumerate().take(self.param_info.len()) {
            let val = slot.value;
            let info = &self.param_info[i];
            let new_label = Some(info.name.clone());

            // Label dirty-check
            if self.label_cache[i] != new_label {
                self.label_cache[i] = new_label;
                if let Some(ref ids) = self.slider_ids[i]
                    && ids.label >= 0
                {
                    tree.set_text(ids.label as u32, &info.name);
                }
            }

            // Value dirty-check
            if val != self.param_cache[i] || self.param_cache[i].is_nan() {
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

    fn sync_values_generator(
        &mut self,
        tree: &mut UITree,
        values: &[manifold_core::effects::ParamSlot],
    ) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| self.find_label_name(label_id))
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        for (i, slot) in values.iter().enumerate().take(self.param_info.len()) {
            let val = slot.value;
            let info = &self.param_info[i];

            // Label dirty-check (slider rows only — toggle/trigger rows have
            // their label baked into the row at build time).
            if !info.is_toggle && !info.is_trigger {
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
            } else if info.is_trigger {
                // Trigger button stays neutral — the counter value isn't
                // user-visible; nothing to re-render per frame.
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

    pub fn sync_effect_name(&mut self, tree: &mut UITree, name: &str) {
        self.name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    pub fn sync_enabled(&mut self, _tree: &mut UITree, enabled: bool) {
        // Just update the field — tree update happens in sync_values() dirty-check.
        self.enabled = enabled;
    }

    pub fn sync_gen_type_name(&mut self, tree: &mut UITree, name: &str) {
        self.name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    /// Update a string param value and its display text (generator kind).
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

    // ── Event handling ────────────────────────────────────────────

    /// Resolve the panel-local positional `pi` back to its stable
    /// [`ParamId`](manifold_core::effects::ParamId) for outbound
    /// [`PanelAction`] emission. The panel's per-widget bookkeeping is
    /// legitimately positional (it indexes `param_info`, `driver_btn_ids`,
    /// etc.); this is the one helper that keeps that off the wire format.
    #[inline]
    fn pid_at(&self, pi: usize) -> manifold_core::effects::ParamId {
        self.param_info[pi].param_id.clone()
    }

    pub fn handle_click(&mut self, node_id: u32) -> Vec<PanelAction> {
        match self.kind {
            ParamCardKind::Effect => self.handle_click_effect(node_id),
            ParamCardKind::Generator => self.handle_click_generator(node_id),
        }
    }

    fn handle_click_effect(&mut self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        let ei = self.effect_index;

        // Header buttons
        if id == self.toggle_btn_id {
            return vec![PanelAction::EffectToggle(ei)];
        }
        if id == self.chevron_btn_id {
            return vec![PanelAction::EffectCollapseToggle(ei)];
        }
        if id == self.cog_btn_id {
            return vec![PanelAction::OpenGraphEditor(ei)];
        }

        // Per-param row elements (D/E buttons, config drawers, label copy) —
        // shared dispatch; map the abstract RowClick to effect-side actions.
        if let Some(rc) = match_param_row_click(
            id,
            &self.driver_btn_ids,
            &self.envelope_btn_ids,
            &self.driver_config_ids,
            &self.envelope_random_config_ids,
            &self.ableton_config_ids,
            &self.slider_ids,
            &self.osc_addresses,
            &self.param_info,
        ) {
            return match rc {
                RowClick::DriverToggle(pi) => {
                    vec![PanelAction::EffectDriverToggle(ei, self.pid_at(pi))]
                }
                RowClick::EnvelopeToggle(pi) => {
                    vec![PanelAction::EffectEnvelopeToggle(ei, self.pid_at(pi))]
                }
                RowClick::DriverConfig(pi, action) => {
                    vec![PanelAction::EffectDriverConfig(ei, self.pid_at(pi), action)]
                }
                RowClick::EnvModeToggle(pi) => {
                    vec![PanelAction::EffectEnvModeToggle(ei, self.pid_at(pi))]
                }
                RowClick::EnvRandomJumpToggle(pi) => {
                    vec![PanelAction::EffectEnvRandomJumpToggle(ei, self.pid_at(pi))]
                }
                RowClick::AbletonInvert(pi) => {
                    vec![PanelAction::AbletonInvertToggle(ei, self.pid_at(pi))]
                }
                RowClick::LabelCopy(pi) => {
                    if let Some(ids) = &self.slider_ids[pi] {
                        self.copied_flash.trigger(ids.label as u32);
                    }
                    let addr = self.osc_addresses[pi].clone().unwrap_or_default();
                    vec![PanelAction::CopyOscAddress(addr)]
                }
            };
        }

        // Card selection — any click on card background, border, or header
        if id == self.border_id
            || id == self.header_bg_id
            || id == self.inner_bg_id
            || id == self.drag_icon_id
            || id == self.name_label_id
        {
            return vec![PanelAction::EffectCardClicked(ei)];
        }

        Vec::new()
    }

    fn handle_click_generator(&mut self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;

        // Chevron → collapse/expand
        if id == self.chevron_btn_id {
            return vec![PanelAction::GenCollapseToggle];
        }

        // Change button → open type picker
        if id == self.change_btn_id {
            return vec![PanelAction::GenTypeClicked(self.layer_id.clone())];
        }

        // Cog → open graph editor for this generator
        if id == self.cog_btn_id {
            return vec![PanelAction::OpenGeneratorGraphEditor];
        }

        // Card click (header bg, name, border) → select the card
        if id == self.header_bg_id || id == self.name_label_id || id == self.border_id {
            return vec![PanelAction::GenCardClicked];
        }

        // Toggle / Trigger buttons — same button slot, different semantics.
        // is_trigger fires GenParamFire (counter +1); is_toggle fires
        // GenParamToggle (0↔1 flip).
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && id == t.button_id
            {
                let is_trigger = self
                    .param_info
                    .get(pi)
                    .map(|i| i.is_trigger)
                    .unwrap_or(false);
                let action = if is_trigger {
                    PanelAction::GenParamFire(self.pid_at(pi))
                } else {
                    PanelAction::GenParamToggle(self.pid_at(pi))
                };
                return vec![action];
            }
        }

        // Per-param row elements (D/E buttons, config drawers, slider-label
        // copy) — shared dispatch; map RowClick to generator-side actions.
        // Toggle/trigger params are skipped for D/E inside the matcher.
        if let Some(rc) = match_param_row_click(
            id,
            &self.driver_btn_ids,
            &self.envelope_btn_ids,
            &self.driver_config_ids,
            &self.envelope_random_config_ids,
            &self.ableton_config_ids,
            &self.slider_ids,
            &self.osc_addresses,
            &self.param_info,
        ) {
            return match rc {
                RowClick::DriverToggle(pi) => vec![PanelAction::GenDriverToggle(self.pid_at(pi))],
                RowClick::EnvelopeToggle(pi) => {
                    vec![PanelAction::GenEnvelopeToggle(self.pid_at(pi))]
                }
                RowClick::DriverConfig(pi, action) => {
                    vec![PanelAction::GenDriverConfig(self.pid_at(pi), action)]
                }
                RowClick::EnvModeToggle(pi) => vec![PanelAction::GenEnvModeToggle(self.pid_at(pi))],
                RowClick::EnvRandomJumpToggle(pi) => {
                    vec![PanelAction::GenEnvRandomJumpToggle(self.pid_at(pi))]
                }
                RowClick::AbletonInvert(pi) => {
                    vec![PanelAction::AbletonGenInvertToggle(self.pid_at(pi))]
                }
                RowClick::LabelCopy(pi) => {
                    if let Some(ids) = &self.slider_ids[pi] {
                        self.copied_flash.trigger(ids.label as u32);
                    }
                    let addr = self.osc_addresses[pi].clone().unwrap_or_default();
                    vec![PanelAction::CopyOscAddress(addr)]
                }
            };
        }

        // Toggle labels → copy OSC address (slider labels handled by the
        // shared matcher above).
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
        match self.kind {
            ParamCardKind::Effect => self.handle_pointer_down_effect(node_id, pos),
            ParamCardKind::Generator => self.handle_pointer_down_generator(node_id, pos),
        }
    }

    fn handle_pointer_down_effect(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        let ei = self.effect_index;

        // Check envelope range handles first (highest priority for Random mode)
        for (pi, range) in self.envelope_range_ids.iter().enumerate() {
            if let Some(t) = range {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_range_param = pi as i32;
                    self.drag.dragging_range_is_min = true;
                    return vec![PanelAction::EffectEnvRangeSnapshot(ei, self.pid_at(pi))];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_range_param = pi as i32;
                    self.drag.dragging_range_is_min = false;
                    return vec![PanelAction::EffectEnvRangeSnapshot(ei, self.pid_at(pi))];
                }
            }
        }

        // Check envelope target bars (ADSR mode)
        for (pi, target) in self.target_ids.iter().enumerate() {
            if let Some(t) = target
                && node_id as i32 == t.target_bar_id
            {
                self.drag.dragging_target_param = pi as i32;
                return vec![PanelAction::EffectTargetSnapshot(ei, self.pid_at(pi))];
            }
        }

        // Check trim bars
        for (pi, trim) in self.trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = true;
                    return vec![PanelAction::EffectTrimSnapshot(ei, self.pid_at(pi))];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = false;
                    return vec![PanelAction::EffectTrimSnapshot(ei, self.pid_at(pi))];
                }
            }
        }

        // Check Ableton trim bars
        for (pi, trim) in self.ableton_trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_ableton_trim_param = pi as i32;
                    self.drag.dragging_ableton_trim_is_min = true;
                    return vec![PanelAction::AbletonTrimSnapshot(ei, self.pid_at(pi))];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_ableton_trim_param = pi as i32;
                    self.drag.dragging_ableton_trim_is_min = false;
                    return vec![PanelAction::AbletonTrimSnapshot(ei, self.pid_at(pi))];
                }
            }
        }

        // Check ADSR slider tracks
        for (pi, env_cfg) in self.envelope_config_ids.iter().enumerate() {
            if let Some(c) = env_cfg {
                if node_id == c.attack_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 0;
                    let norm = BitmapSlider::x_to_normalized(c.attack_slider.track_rect, pos.x);
                    return vec![
                        PanelAction::EffectEnvParamSnapshot(ei, self.pid_at(pi)),
                        PanelAction::EffectEnvParamChanged(
                            ei,
                            self.pid_at(pi),
                            EnvelopeParam::Attack,
                            norm * ENV_ADR_MAX,
                        ),
                    ];
                }
                if node_id == c.decay_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 1;
                    let norm = BitmapSlider::x_to_normalized(c.decay_slider.track_rect, pos.x);
                    return vec![
                        PanelAction::EffectEnvParamSnapshot(ei, self.pid_at(pi)),
                        PanelAction::EffectEnvParamChanged(
                            ei,
                            self.pid_at(pi),
                            EnvelopeParam::Decay,
                            norm * ENV_ADR_MAX,
                        ),
                    ];
                }
                if node_id == c.sustain_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 2;
                    let norm = BitmapSlider::x_to_normalized(c.sustain_slider.track_rect, pos.x);
                    return vec![
                        PanelAction::EffectEnvParamSnapshot(ei, self.pid_at(pi)),
                        PanelAction::EffectEnvParamChanged(
                            ei,
                            self.pid_at(pi),
                            EnvelopeParam::Sustain,
                            norm * ENV_S_MAX,
                        ),
                    ];
                }
                if node_id == c.release_slider.track {
                    self.drag.dragging_env_param = pi as i32;
                    self.drag.dragging_env_slot = 3;
                    let norm = BitmapSlider::x_to_normalized(c.release_slider.track_rect, pos.x);
                    return vec![
                        PanelAction::EffectEnvParamSnapshot(ei, self.pid_at(pi)),
                        PanelAction::EffectEnvParamChanged(
                            ei,
                            self.pid_at(pi),
                            EnvelopeParam::Release,
                            norm * ENV_ADR_MAX,
                        ),
                    ];
                }
            }
        }

        // Check param slider tracks.
        // When a driver is active, check if click is near a trim handle first —
        // the 4px trim bars are hard to hit precisely, so we use a proximity zone.
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let Some(ids) = slider
                && (node_id == ids.track || {
                    // Also accept clicks on trim bar / fill / target nodes that are children of this track
                    self.trim_ids
                        .get(pi)
                        .and_then(|t| t.as_ref())
                        .is_some_and(|t| {
                            node_id as i32 == t.fill_id
                                || node_id as i32 == t.min_bar_id
                                || node_id as i32 == t.max_bar_id
                        })
                        || self
                            .target_ids
                            .get(pi)
                            .and_then(|t| t.as_ref())
                            .is_some_and(|t| node_id as i32 == t.target_bar_id)
                        || self
                            .envelope_range_ids
                            .get(pi)
                            .and_then(|t| t.as_ref())
                            .is_some_and(|t| {
                                node_id as i32 == t.fill_id
                                    || node_id as i32 == t.min_bar_id
                                    || node_id as i32 == t.max_bar_id
                            })
                })
            {
                // If driver is expanded, check proximity to trim handles before falling through to param drag
                if self
                    .state
                    .mod_state
                    .driver_expanded
                    .get(pi)
                    .copied()
                    .unwrap_or(false)
                    && let Some(ref trim) = self.trim_ids.get(pi).and_then(|t| t.as_ref())
                {
                    let usable = ids.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = ids.track_rect.x + OVERLAY_INSET;
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
                    let min_center = base_x + tmin * usable;
                    let max_center = base_x + tmax * usable;
                    let hit_zone = 8.0; // px proximity zone for trim handles

                    let dist_min = (pos.x - min_center).abs();
                    let dist_max = (pos.x - max_center).abs();

                    if dist_min < hit_zone && dist_min <= dist_max {
                        self.drag.dragging_trim_param = pi as i32;
                        self.drag.dragging_trim_is_min = true;
                        let _ = trim;
                        return vec![PanelAction::EffectTrimSnapshot(ei, self.pid_at(pi))];
                    }
                    if dist_max < hit_zone {
                        self.drag.dragging_trim_param = pi as i32;
                        self.drag.dragging_trim_is_min = false;
                        return vec![PanelAction::EffectTrimSnapshot(ei, self.pid_at(pi))];
                    }
                }

                // If envelope is expanded, check proximity to target/range handles
                if self
                    .state
                    .mod_state
                    .envelope_expanded
                    .get(pi)
                    .copied()
                    .unwrap_or(false)
                {
                    let usable = ids.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = ids.track_rect.x + OVERLAY_INSET;
                    let hit_zone = 8.0;
                    let env_mode = self
                        .state
                        .mod_state
                        .env_mode
                        .get(pi)
                        .copied()
                        .unwrap_or(EnvelopeMode::Adsr);

                    if env_mode == EnvelopeMode::Random {
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
                        let min_center = base_x + rmin * usable;
                        let max_center = base_x + rmax * usable;
                        let dist_min = (pos.x - min_center).abs();
                        let dist_max = (pos.x - max_center).abs();

                        if dist_min < hit_zone && dist_min <= dist_max {
                            self.drag.dragging_range_param = pi as i32;
                            self.drag.dragging_range_is_min = true;
                            return vec![PanelAction::EffectEnvRangeSnapshot(ei, self.pid_at(pi))];
                        }
                        if dist_max < hit_zone {
                            self.drag.dragging_range_param = pi as i32;
                            self.drag.dragging_range_is_min = false;
                            return vec![PanelAction::EffectEnvRangeSnapshot(ei, self.pid_at(pi))];
                        }
                    } else {
                        let tgt = self
                            .state
                            .mod_state
                            .target_norm
                            .get(pi)
                            .copied()
                            .unwrap_or(1.0);
                        let target_center = base_x + tgt * usable;

                        if (pos.x - target_center).abs() < hit_zone {
                            self.drag.dragging_target_param = pi as i32;
                            return vec![PanelAction::EffectTargetSnapshot(ei, self.pid_at(pi))];
                        }
                    }
                }

                // No trim/target nearby — normal param slider drag
                self.drag.dragging_param = pi as i32;
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let info = &self.param_info[pi];
                let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                let val = if info.whole_numbers { val.round() } else { val };
                return vec![
                    PanelAction::EffectParamSnapshot(ei, self.pid_at(pi)),
                    PanelAction::EffectParamChanged(ei, self.pid_at(pi), val),
                ];
            }
        }

        Vec::new()
    }

    fn handle_pointer_down_generator(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        // Check envelope range handles (Random mode)
        for (pi, range) in self.envelope_range_ids.iter().enumerate() {
            if let Some(t) = range {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_range_param = pi as i32;
                    self.drag.dragging_range_is_min = true;
                    return vec![PanelAction::GenEnvRangeSnapshot(self.pid_at(pi))];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_range_param = pi as i32;
                    self.drag.dragging_range_is_min = false;
                    return vec![PanelAction::GenEnvRangeSnapshot(self.pid_at(pi))];
                }
            }
        }

        // Check envelope targets (ADSR mode)
        for (pi, target) in self.target_ids.iter().enumerate() {
            if let Some(t) = target
                && node_id as i32 == t.target_bar_id
            {
                self.drag.dragging_target_param = pi as i32;
                return vec![PanelAction::GenTargetSnapshot(self.pid_at(pi))];
            }
        }

        // Check trim bars
        for (pi, trim) in self.trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = true;
                    return vec![PanelAction::GenTrimSnapshot(self.pid_at(pi))];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_trim_param = pi as i32;
                    self.drag.dragging_trim_is_min = false;
                    return vec![PanelAction::GenTrimSnapshot(self.pid_at(pi))];
                }
            }
        }

        // Check Ableton trim bars
        for (pi, trim) in self.ableton_trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.drag.dragging_ableton_trim_param = pi as i32;
                    self.drag.dragging_ableton_trim_is_min = true;
                    return vec![PanelAction::AbletonGenTrimSnapshot(self.pid_at(pi))];
                }
                if node_id as i32 == t.max_bar_id {
                    self.drag.dragging_ableton_trim_param = pi as i32;
                    self.drag.dragging_ableton_trim_is_min = false;
                    return vec![PanelAction::AbletonGenTrimSnapshot(self.pid_at(pi))];
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
                            PanelAction::GenEnvParamSnapshot(self.pid_at(pi)),
                            PanelAction::GenEnvParamChanged(self.pid_at(pi), *param, norm * max),
                        ];
                    }
                }
            }
        }

        // Check param slider tracks (skip toggles + triggers — neither has a
        // slider widget on the row)
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if self
                .param_info
                .get(pi)
                .map(|i| i.is_toggle || i.is_trigger)
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
                    PanelAction::GenParamSnapshot(self.pid_at(pi)),
                    PanelAction::GenParamChanged(self.pid_at(pi), val),
                ];
            }
        }

        Vec::new()
    }

    /// Drag-move dispatch. The state mutation + tree repositioning is identical
    /// for both kinds; only the emitted [`PanelAction`] variant differs, so the
    /// body is shared and branches on `kind` at each emission point.
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        // Range handle drag — update state, reposition bar nodes, dispatch action
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

                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::EffectEnvRangeChanged(ei, pid, new_min, new_max)]
                    }
                    ParamCardKind::Generator => {
                        vec![PanelAction::GenEnvRangeChanged(pid, new_min, new_max)]
                    }
                };
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

                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::EffectTargetChanged(ei, pid, norm)],
                    ParamCardKind::Generator => vec![PanelAction::GenTargetChanged(pid, norm)],
                };
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

                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::EffectTrimChanged(ei, pid, new_min, new_max)]
                    }
                    ParamCardKind::Generator => {
                        vec![PanelAction::GenTrimChanged(pid, new_min, new_max)]
                    }
                };
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

                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::AbletonTrimChanged(ei, pid, new_min, new_max)]
                    }
                    ParamCardKind::Generator => {
                        vec![PanelAction::AbletonGenTrimChanged(pid, new_min, new_max)]
                    }
                };
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
                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::EffectEnvParamChanged(ei, pid, param, val)]
                    }
                    ParamCardKind::Generator => {
                        vec![PanelAction::GenEnvParamChanged(pid, param, val)]
                    }
                };
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
                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::EffectParamChanged(ei, pid, val)],
                    ParamCardKind::Generator => vec![PanelAction::GenParamChanged(pid, val)],
                };
            }
        }

        Vec::new()
    }

    /// Drag-end dispatch — commit the active drag. Identical bookkeeping for
    /// both kinds; only the emitted [`PanelAction`] variant differs.
    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        if self.drag.dragging_range_param >= 0 {
            let pi = self.drag.dragging_range_param as usize;
            self.drag.dragging_range_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::EffectEnvRangeCommit(ei, pid)],
                ParamCardKind::Generator => vec![PanelAction::GenEnvRangeCommit(pid)],
            };
        }
        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            self.drag.dragging_target_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::EffectTargetCommit(ei, pid)],
                ParamCardKind::Generator => vec![PanelAction::GenTargetCommit(pid)],
            };
        }
        if self.drag.dragging_trim_param >= 0 {
            let pi = self.drag.dragging_trim_param as usize;
            self.drag.dragging_trim_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::EffectTrimCommit(ei, pid)],
                ParamCardKind::Generator => vec![PanelAction::GenTrimCommit(pid)],
            };
        }
        if self.drag.dragging_ableton_trim_param >= 0 {
            let pi = self.drag.dragging_ableton_trim_param as usize;
            self.drag.dragging_ableton_trim_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::AbletonTrimCommit(ei, pid)],
                ParamCardKind::Generator => vec![PanelAction::AbletonGenTrimCommit(pid)],
            };
        }
        if self.drag.dragging_env_param >= 0 {
            let pi = self.drag.dragging_env_param as usize;
            self.drag.dragging_env_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::EffectEnvParamCommit(ei, pid)],
                ParamCardKind::Generator => vec![PanelAction::GenEnvParamCommit(pid)],
            };
        }
        if self.drag.dragging_param >= 0 {
            let pi = self.drag.dragging_param as usize;
            self.drag.dragging_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::EffectParamCommit(ei, pid)],
                ParamCardKind::Generator => vec![PanelAction::GenParamCommit(pid)],
            };
        }

        Vec::new()
    }

    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        match self.kind {
            ParamCardKind::Effect => self.handle_right_click_effect(node_id),
            ParamCardKind::Generator => self.handle_right_click_generator(node_id),
        }
    }

    fn handle_right_click_effect(&self, node_id: u32) -> Vec<PanelAction> {
        let ei = self.effect_index;
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let Some(ids) = slider {
                // Right-click slider track → reset to default
                if node_id == ids.track {
                    let default = self.param_info.get(pi).map(|i| i.default).unwrap_or(0.0);
                    return vec![PanelAction::EffectParamRightClick(ei, self.pid_at(pi), default)];
                }
                // Right-click label → map to macro
                if ids.label >= 0 && node_id == ids.label as u32 {
                    return vec![PanelAction::EffectParamLabelRightClick(ei, self.pid_at(pi))];
                }
            }
        }
        Vec::new()
    }

    fn handle_right_click_generator(&self, node_id: u32) -> Vec<PanelAction> {
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
                .map(|i| i.is_toggle || i.is_trigger)
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(ids) = slider {
                // Right-click slider track → reset to default
                if node_id == ids.track {
                    let default = self.param_info.get(pi).map(|i| i.default).unwrap_or(0.0);
                    return vec![PanelAction::GenParamRightClick(self.pid_at(pi), default)];
                }
                // Right-click label → map to macro
                if ids.label >= 0 && node_id == ids.label as u32 {
                    return vec![PanelAction::GenParamLabelRightClick(self.pid_at(pi))];
                }
            }
        }
        Vec::new()
    }
}

impl Default for ParamCardPanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    // ── Effect-card fixtures + tests ──────────────────────────────

    fn effect_config() -> ParamCardConfig {
        let n = 2;
        ParamCardConfig {
            kind: ParamCardKind::Effect,
            effect_index: 0,
            effect_id: EffectId::new("test-effect-0"),
            name: "Blur".into(),
            enabled: true,
            collapsed: false,
            supports_envelopes: true,
            string_params: Vec::new(),
            layer_id: None,
            params: vec![
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("radius"),
                    name: "Radius".into(),
                    min: 0.0,
                    max: 100.0,
                    default: 10.0,
                    whole_numbers: true,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("strength"),
                    name: "Strength".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    whole_numbers: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
            ],
            has_drv: false,
            has_env: false,
            has_abl: false,
            has_graph_mod: false,
            driver_active: vec![false; n],
            envelope_active: vec![false; n],
            trim_min: vec![0.0; n],
            trim_max: vec![1.0; n],
            target_norm: vec![1.0; n],
            env_attack: vec![0.0; n],
            env_decay: vec![0.0; n],
            env_sustain: vec![0.0; n],
            env_release: vec![0.0; n],
            env_mode: vec![EnvelopeMode::Adsr; n],
            env_random_jump: vec![false; n],
            env_range_min: vec![0.0; n],
            env_range_max: vec![1.0; n],
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
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
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
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.toggle_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectToggle(0)));
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.chevron_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectCollapseToggle(0)));
    }

    #[test]
    fn handle_click_driver_button() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.driver_btn_ids[0] as u32);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::EffectDriverToggle(ei, param_id) => {
                assert_eq!(*ei, 0);
                assert_eq!(param_id.as_ref(), "radius");
            }
            other => panic!("expected EffectDriverToggle, got {:?}", other),
        }
    }

    #[test]
    fn sync_values_updates_slider() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        tree.clear_dirty();
        use manifold_core::effects::ParamSlot;
        panel.sync_values(
            &mut tree,
            &[ParamSlot::exposed(50.0), ParamSlot::exposed(0.8)],
        );
        assert!(tree.has_dirty());
    }

    #[test]
    fn compute_height_collapsed() {
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());

        let expanded_h = panel.compute_height();
        panel.set_collapsed(true);
        let collapsed_h = panel.compute_height();

        assert!(collapsed_h < expanded_h);
    }

    #[test]
    fn effect_card_with_driver_expanded() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.driver_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.driver_config_ids[0].is_some());
        assert!(panel.trim_ids[0].is_some());
    }

    #[test]
    fn effect_card_with_envelope_expanded() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.envelope_config_ids[0].is_some());
        assert!(panel.target_ids[0].is_some());
    }

    // ── Generator-card fixtures + tests ───────────────────────────

    fn gen_config() -> ParamCardConfig {
        ParamCardConfig {
            kind: ParamCardKind::Generator,
            name: "Plasma".into(),
            collapsed: false,
            effect_index: 0,
            effect_id: EffectId::new(""),
            enabled: true,
            supports_envelopes: true,
            has_drv: false,
            has_env: false,
            has_abl: false,
            has_graph_mod: false,
            layer_id: None,
            params: vec![
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("speed"),
                    name: "Speed".into(),
                    min: 0.0,
                    max: 10.0,
                    default: 1.0,
                    whole_numbers: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("invert"),
                    name: "Invert".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    whole_numbers: false,
                    exposed: true,
                    is_toggle: true,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                },
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("scale"),
                    name: "Scale".into(),
                    min: 0.1,
                    max: 5.0,
                    default: 1.0,
                    whole_numbers: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
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
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.border_id >= 0);
        assert!(panel.name_label_id >= 0);
        assert!(panel.chevron_btn_id >= 0);
        assert!(panel.slider_ids[0].is_some()); // Speed = slider
        assert!(panel.toggle_ids[1].is_some()); // Invert = toggle
        assert!(panel.slider_ids[2].is_some()); // Scale = slider
        assert!(panel.node_count > 0);
    }

    #[test]
    fn handle_click_gen_type() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
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
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let toggle = panel.toggle_ids[1].as_ref().unwrap();
        let actions = panel.handle_click(toggle.button_id as u32);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::GenParamToggle(param_id) => {
                assert_eq!(param_id.as_ref(), "invert");
            }
            other => panic!("expected GenParamToggle, got {:?}", other),
        }
    }

    #[test]
    fn gen_sync_values_updates() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        tree.clear_dirty();
        panel.sync_values(
            &mut tree,
            &[
                manifold_core::effects::ParamSlot::exposed(5.0),
                manifold_core::effects::ParamSlot::exposed(1.0),
                manifold_core::effects::ParamSlot::exposed(2.5),
            ],
        );
        assert!(tree.has_dirty());
    }

    #[test]
    fn gen_compute_height_with_driver_expanded() {
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());

        let base_h = panel.compute_height();
        panel.state.mod_state.driver_expanded[0] = true;
        let expanded_h = panel.compute_height();

        assert!(expanded_h > base_h);
        assert!((expanded_h - base_h - DRIVER_CONFIG_HEIGHT).abs() < 0.1);
    }
}
