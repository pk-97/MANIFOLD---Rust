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
use super::{AudioShapeParam, GraphParamTarget, PanelAction, TrimKind};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use manifold_core::{EffectId, LayerId};

/// Map a 0..1 slider position to an [`AudioModShape`] scalar's value, using the
/// per-control full-scale constants. The single conversion shared by the audio
/// shaping sliders' mouse-down and drag paths.
fn audio_shape_value_from_norm(which: AudioShapeParam, norm: f32) -> f32 {
    let n = norm.clamp(0.0, 1.0);
    match which {
        AudioShapeParam::Sensitivity => n * AUDIO_SENS_MAX,
        AudioShapeParam::Attack => n * AUDIO_ATTACK_MAX_MS,
        AudioShapeParam::Release => n * AUDIO_RELEASE_MAX_MS,
    }
}

/// Display text for an audio shaping slider's value field.
fn audio_shape_value_text(which: AudioShapeParam, value: f32) -> String {
    match which {
        AudioShapeParam::Sensitivity => format!("{value:.2}"),
        AudioShapeParam::Attack | AudioShapeParam::Release => format!("{value:.0} ms"),
    }
}

/// Which kind of preset a card is displaying. Carries the small set of real
/// behavioral differences between the effect and generator inspector cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamCardKind {
    Effect,
    Generator,
}

/// Where the card is being shown — which decides its chrome, not its data.
///
/// `Perform` is the inspector / live surface: the full performing card with its
/// drag-reorder handle, the "open graph editor" cog, and the right-click
/// perform-mapping menu. `Author` is the graph editor's left lane: the same
/// instrument, but the perform-only chrome is suppressed (you're already in the
/// editor, reorder is meaningless against one card, and the perform-mapping menu
/// is replaced by the sideways mapping drawer) and each mappable row gains a
/// chevron at its right edge that opens that drawer. Default is `Perform` so
/// every existing inspector card is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardContext {
    Perform,
    Author,
}

/// Per-parameter configuration info provided by the app layer. One per slot
/// in the host's `param_values`, in declaration order (static prefix, then
/// user-exposed tail for effects).
#[derive(Debug, Clone)]
pub struct ParamInfo {
    /// Stable [`ParamId`](manifold_core::effects::ParamId) for this slot — for
    /// static-tier params the `&'static str` declared in the preset's
    /// `ParamSpec`; for user-tier (graph-editor-exposed) effect params the
    /// owned id from `PresetInstance.user_param_bindings[j].id`. Carried on
    /// the wire when a widget emits a [`PanelAction`](super::PanelAction) so
    /// the bridge never does a positional `pi → ParamId` lookup.
    pub param_id: manifold_core::effects::ParamId,
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub whole_numbers: bool,
    /// Angle presentation hint. Storage stays radians (drivers / Ableton /
    /// envelopes write radians every frame, unchanged); the slider value cell
    /// displays and reads back DEGREES, converting only at the text boundary.
    /// Mirrors `whole_numbers` as a display-only flag. See `ParamType::Angle`.
    pub is_angle: bool,
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
    /// This row carries a per-instance editable reshape (range / scale / offset
    /// / invert / curve). After the card-target unification every exposed card
    /// param is remappable — effect static + user-tail bindings AND generator
    /// params — via `EditParamMappingCommand` on the watched graph target, so
    /// `editor_card_config` sets this `true` for both kinds. Drives the sideways
    /// mapping-drawer chevron, which only appears in [`CardContext::Author`].
    pub mappable: bool,
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
    /// (`PresetInstance.graph.is_some()`) — drives the pink "MOD" badge +
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
    /// Per-param envelope target (the orange handle, normalized). Default 1.0.
    pub target_norm: Vec<f32>,
    /// Per-param envelope decay time in beats. Default 1.0.
    pub env_decay: Vec<f32>,
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
    /// Audio-modulation state (per-param active/send/feature + card-level send
    /// list). Bundled so the config grows by one field.
    pub audio: super::param_slider_shared::AudioCardState,
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
/// Width of the right-edge mapping-drawer chevron lane (Author context). Rows
/// that show it shrink their slider by this much so the chevron sits past the
/// D/E buttons at the row's right edge.
const MAP_CHEVRON_W: f32 = 14.0;

// Effect shell furniture.
const DRAG_HANDLE_W: f32 = 18.0;
const TOGGLE_W: f32 = 30.0;
// 3-letter chips (ABL/ENV/DRV/MOD) at FONT_CAPTION don't need 36px; the
// narrower chip reclaims header width for the effect name when several show.
const BADGE_W: f32 = 28.0;
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
    label_id: i32,
    button_id: i32,
}

/// Packed right-aligned positions for the 0–4 header modulation badges.
/// In display order [MOD, ABL, ENV, DRV]; `None` for a hidden badge.
/// `name_right` is the right edge of the name cell (left edge of the badge
/// block, or the toggle gap when no badge shows) — what `name_w` clips to.
struct BadgeLayout {
    mod_x: Option<f32>,
    abl_x: Option<f32>,
    env_x: Option<f32>,
    drv_x: Option<f32>,
    name_right: f32,
}

/// Lay the visible header badges out flush against the left edge of the toggle,
/// packing only the active ones so a lone badge sits at the right edge instead
/// of floating mid-header in a slot reserved for badges that aren't showing.
fn effect_badge_layout(
    toggle_x: f32,
    show_mod: bool,
    show_abl: bool,
    show_env: bool,
    show_drv: bool,
) -> BadgeLayout {
    let shows = [show_mod, show_abl, show_env, show_drv];
    let count = shows.iter().filter(|s| **s).count();
    let block_right = toggle_x - GAP;
    let block_w = if count == 0 {
        0.0
    } else {
        count as f32 * BADGE_W + (count as f32 - 1.0) * GAP
    };
    let block_left = block_right - block_w;
    let mut xs: [Option<f32>; 4] = [None; 4];
    let mut cursor = block_left;
    for (i, show) in shows.iter().enumerate() {
        if *show {
            xs[i] = Some(cursor);
            cursor += BADGE_W + GAP;
        }
    }
    BadgeLayout {
        mod_x: xs[0],
        abl_x: xs[1],
        env_x: xs[2],
        drv_x: xs[3],
        name_right: if count == 0 { block_right } else { block_left - GAP },
    }
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
    /// Perform (inspector) vs Author (graph editor). Decides chrome only — the
    /// data contract is identical. Defaults to `Perform`.
    context: CardContext,

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
    /// Transparent CLIPS_CHILDREN container sized to the name cell. The name
    /// label nests inside it so a long effect name is clipped at the cell edge
    /// instead of overflowing into the header badges. Resized live in sync when
    /// the active-badge set changes.
    name_clip_id: i32,
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
    /// Per-param transparent full-row hit catcher behind the slider widgets.
    /// Carries the param's right-click menu intent so the value cell + gaps
    /// resolve to the param menu (track stays instant-reset). -1 if unbuilt.
    row_catcher_ids: Vec<i32>,
    driver_btn_ids: Vec<i32>,
    envelope_btn_ids: Vec<i32>,
    driver_config_ids: Vec<Option<DriverConfigIds>>,
    /// Per-param "A" audio-mod button node id.
    audio_btn_ids: Vec<i32>,
    /// Per-param audio drawer ids + send count (for click resolution).
    audio_configs: Vec<Option<(crate::panels::drawer::DrawerIds, usize)>>,
    /// Per-param orange envelope target handle on the slider track (when armed).
    target_ids: Vec<Option<EnvelopeTargetIds>>,
    /// Per-param envelope drawer — the single "Decay" slider (when armed).
    envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    trim_ids: Vec<Option<TrimHandleIds>>,
    ableton_trim_ids: Vec<Option<TrimHandleIds>>,
    /// Per-param green audio-mod trim handles (when an audio mod is armed).
    audio_trim_ids: Vec<Option<TrimHandleIds>>,
    ableton_config_ids: Vec<Option<AbletonConfigIds>>,

    // ── Node IDs — per-param (generator) ──
    toggle_ids: Vec<Option<ToggleParamIds>>,
    string_param_btn_ids: Vec<i32>,

    /// Per-param sideways-mapping-drawer chevron (Author context, mappable rows
    /// only). `-1` for rows without one. Indexed by param index.
    mapping_chevron_ids: Vec<i32>,

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
            context: CardContext::Perform,
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
            name_clip_id: -1,
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
            row_catcher_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            audio_btn_ids: Vec::new(),
            audio_configs: Vec::new(),
            target_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            ableton_trim_ids: Vec::new(),
            audio_trim_ids: Vec::new(),
            ableton_config_ids: Vec::new(),
            toggle_ids: Vec::new(),
            string_param_btn_ids: Vec::new(),
            mapping_chevron_ids: Vec::new(),
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
            &config.env_decay,
            &config.driver_beat_div_idx,
            &config.driver_waveform_idx,
            &config.driver_reversed,
            &config.driver_dotted,
            &config.driver_triplet,
        );
        self.state.mod_state.sync_audio(n, &config.audio);
        self.osc_addresses = config
            .params
            .iter()
            .map(|p| p.osc_address.clone())
            .collect();
        self.copied_flash.clear();
        self.slider_ids = vec![None; n];
        self.row_catcher_ids = vec![-1; n];
        self.driver_btn_ids = vec![-1; n];
        self.envelope_btn_ids = vec![-1; n];
        self.driver_config_ids = Vec::new();
        self.driver_config_ids.resize_with(n, || None);
        self.audio_btn_ids = vec![-1; n];
        self.audio_configs = Vec::new();
        self.audio_configs.resize_with(n, || None);
        self.target_ids = Vec::new();
        self.target_ids.resize_with(n, || None);
        self.envelope_config_ids = Vec::new();
        self.envelope_config_ids.resize_with(n, || None);
        self.trim_ids = Vec::new();
        self.trim_ids.resize_with(n, || None);
        self.ableton_trim_ids = Vec::new();
        self.ableton_trim_ids.resize_with(n, || None);
        self.audio_trim_ids = Vec::new();
        self.audio_trim_ids.resize_with(n, || None);
        self.ableton_config_ids = Vec::new();
        self.ableton_config_ids.resize_with(n, || None);
        self.toggle_ids = Vec::new();
        self.toggle_ids.resize_with(n, || None);
        self.mapping_chevron_ids = vec![-1; n];
        self.string_param_btn_ids = vec![-1; config.string_params.len()];
        self.param_cache = vec![f32::NAN; n];
        self.toggle_cache = vec![false; n];
        self.label_cache = vec![None; n];
    }

    // ── Accessors ─────────────────────────────────────────────────

    pub fn effect_index(&self) -> usize {
        self.effect_index
    }

    /// The [`GraphParamTarget`] this card's per-param actions carry — the
    /// effect index for an effect card, `Generator` for a generator card.
    fn param_target(&self) -> GraphParamTarget {
        match self.kind {
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_index),
            ParamCardKind::Generator => GraphParamTarget::Generator,
        }
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

    /// Set the chrome context (Perform vs Author). Author suppresses the cog /
    /// drag-reorder handle / perform-mapping menu and adds the sideways
    /// mapping-drawer chevron on mappable rows. Takes effect on the next
    /// [`build`](Self::build); the host sets it once on its dedicated panel.
    pub fn set_context(&mut self, context: CardContext) {
        self.context = context;
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

    /// Screen-space rect of the mapping-drawer chevron for the binding with
    /// `param_id` (Author context). The host anchors the sideways drawer beside
    /// it. `None` when the param isn't mappable, isn't built, or no chevron was
    /// drawn (Perform context).
    pub fn mapping_chevron_rect(&self, tree: &UITree, param_id: &str) -> Option<Rect> {
        let pi = self.param_info.iter().position(|p| p.param_id == param_id)?;
        let cid = *self.mapping_chevron_ids.get(pi)?;
        (cid >= 0).then(|| tree.get_bounds(cid as u32))
    }

    /// Hit-test the param NAME labels (slider + toggle/trigger rows) and return
    /// the [`ParamId`](manifold_core::effects::ParamId) of the row whose label
    /// contains `(sx, sy)`, or `None`. Read-only — no behaviour change and no
    /// effect on the performance card; the graph-editor host calls it in Author
    /// context to jump from a card param straight to the node that defines it.
    pub fn label_hit(
        &self,
        tree: &UITree,
        sx: f32,
        sy: f32,
    ) -> Option<manifold_core::effects::ParamId> {
        let pos = Vec2::new(sx, sy);
        for (i, info) in self.param_info.iter().enumerate() {
            let label_id = self
                .slider_ids
                .get(i)
                .and_then(|s| s.as_ref())
                .map(|ids| ids.label)
                .filter(|&l| l >= 0)
                .or_else(|| {
                    self.toggle_ids
                        .get(i)
                        .and_then(|t| t.as_ref())
                        .map(|ids| ids.label_id)
                        .filter(|&l| l >= 0)
                });
            if let Some(lid) = label_id
                && tree.get_bounds(lid as u32).contains(pos)
            {
                return Some(info.param_id.clone());
            }
        }
        None
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
        // An armed envelope adds a single "Decay" slider drawer (the target is
        // an overlay handle on the track, so only the drawer adds height).
        if self
            .state
            .mod_state
            .envelope_expanded
            .get(i)
            .copied()
            .unwrap_or(false)
        {
            h += ENV_CONFIG_HEIGHT;
        }
        if self.param_info[i].ableton_display.is_some() {
            h += ABL_CONFIG_HEIGHT;
        }
        // The audio-modulation drawer auto-shows while a mod is armed (same gate
        // `build_param_row` uses), so reserve its height — otherwise the card is
        // too short and the drawer draws past its bounds into the next row.
        if self
            .state
            .mod_state
            .audio_active
            .get(i)
            .copied()
            .unwrap_or(false)
        {
            h += crate::panels::param_slider_shared::audio_config_height();
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

        // Layout (right-to-left for fixed elements). Badges pack flush against
        // the toggle — only the active ones take a slot — so the name cell is
        // as wide as possible and a lone badge never floats mid-header.
        let cog_x = x + w - PADDING - COG_W;
        let chevron_x = cog_x - GAP - CHEVRON_W;
        let toggle_x = chevron_x - GAP - TOGGLE_W;
        let badges = effect_badge_layout(
            toggle_x,
            self.state.has_graph_mod,
            self.state.has_abl,
            self.state.has_env,
            self.state.has_drv,
        );
        // Author mode drops the drag-reorder handle (one card, nothing to
        // reorder against), so the name reclaims its indent.
        let author = self.context == CardContext::Author;
        let name_x = if author {
            x + PADDING
        } else {
            x + PADDING + DRAG_HANDLE_W + GAP
        };
        let name_w = (badges.name_right - name_x).max(10.0);
        // Placeholder x for a hidden badge — invisible, repositioned by sync
        // when it later turns on. Parking it at the block's right edge keeps it
        // off the name even in the brief frame before sync runs.
        let badge_park = toggle_x - GAP - BADGE_W;
        let mod_x = badges.mod_x.unwrap_or(badge_park);
        let abl_x = badges.abl_x.unwrap_or(badge_park);
        let env_x = badges.env_x.unwrap_or(badge_park);
        let drv_x = badges.drv_x.unwrap_or(badge_park);
        let elem_y = y + (HEADER_HEIGHT - 16.0) * 0.5;
        let badge_y = y + (HEADER_HEIGHT - BADGE_H) * 0.5;

        // Drag handle (hamburger icon drawn as 3 horizontal bars). Perform only
        // — leaving `drag_icon_id = -1` in Author also disables `is_drag_handle`,
        // so the card can't be picked up for a reorder it has no list to join.
        if !author {
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

        // Name cell — a transparent clip container the label nests in, so a
        // long name is cut at the cell edge rather than drawn over the badges.
        self.name_clip_id = tree.add_panel(
            self.header_bg_id,
            name_x,
            elem_y,
            name_w,
            16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                ..UIStyle::default()
            },
        ) as i32;
        tree.set_flag(self.name_clip_id as u32, UIFlags::CLIPS_CHILDREN);

        // Name label — generous width so left-aligned text never self-limits;
        // the clip container above enforces the cell edge.
        self.name_label_id = tree.add_label(
            self.name_clip_id,
            name_x,
            elem_y,
            (toggle_x - name_x).max(name_w),
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
        // Perform only: in Author you're already in the editor, so the cog is
        // suppressed (and `cog_btn_id` stays -1, so its click arm never fires).
        if !author {
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

    fn build_effect_sliders(
        &mut self,
        tree: &mut UITree,
        parent: i32,
        x: f32,
        start_y: f32,
        w: f32,
    ) {
        let mut cy = start_y;
        // Author mode reserves a uniform right-edge lane for the mapping-drawer
        // chevron (drawn only on mappable rows, but reserved on all so slider
        // widths stay even). Perform mode keeps the full slider width.
        let author = self.context == CardContext::Author;
        let chevron_lane = if author {
            MAP_CHEVRON_W + DE_BUTTON_GAP
        } else {
            0.0
        };
        // Label column grows with the row so a wide inspector card gives the
        // param name more room (not just a longer track). Floored at the
        // default, so narrow timeline cards keep the timeline's width exactly.
        let label_width = crate::slider::label_width_for_row(w - PADDING * 2.0);
        // Three buttons in the lane now: E (envelope), → (driver), A (audio).
        let slider_w = w - PADDING * 2.0 - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 3.0 - chevron_lane;

        for i in 0..self.param_info.len() {
            // Hidden params: leave slider_ids[i] = None and skip widget
            // construction entirely. Slot-index semantics for any attached
            // driver/Ableton mapping/envelope are preserved.
            if !self.param_info[i].exposed {
                continue;
            }
            let info = self.param_info[i].clone();
            let row_y = cy;
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
                label_width,
            );
            self.slider_ids[i] = row.slider;
            self.row_catcher_ids[i] = row.row_catcher;
            self.trim_ids[i] = row.trim;
            self.target_ids[i] = row.target;
            self.envelope_config_ids[i] = row.envelope_config;
            self.ableton_trim_ids[i] = row.ableton_trim;
            self.audio_trim_ids[i] = row.audio_trim;
            self.envelope_btn_ids[i] = row.envelope_btn;
            self.driver_btn_ids[i] = row.driver_btn;
            self.driver_config_ids[i] = row.driver_config;
            self.ableton_config_ids[i] = row.ableton_config;
            self.audio_btn_ids[i] = row.audio_btn;
            self.audio_configs[i] = row.audio_config;
            // Mapping-drawer chevron at the row's right edge (Author + mappable).
            // A subtle ">" that opens the sideways range/scale/offset/invert/
            // curve drawer for this binding. Sits past the D/E buttons in the
            // reserved lane; click resolves via `mapping_chevron_ids`.
            if author && info.mappable {
                let ch_x = x + PADDING + (w - PADDING * 2.0) - MAP_CHEVRON_W;
                let ch_y = row_y + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
                self.mapping_chevron_ids[i] = tree.add_button(
                    parent,
                    ch_x,
                    ch_y,
                    MAP_CHEVRON_W,
                    DE_BUTTON_SIZE,
                    UIStyle {
                        bg_color: Color32::TRANSPARENT,
                        hover_bg_color: color::HOVER_OVERLAY,
                        pressed_bg_color: color::PRESS_OVERLAY,
                        text_color: color::CHEVRON_COLOR,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Center,
                        corner_radius: 2.0,
                        ..UIStyle::default()
                    },
                    "\u{203A}", // ›
                ) as i32;
            }
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
        // Perform only: in Author (editor) you're already in the editor.
        let elem_y = inner_y;
        if self.context == CardContext::Perform {
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
            // Author mode reserves the same right-edge mapping-drawer chevron
            // lane the effect card does, so generator slider rows shrink to
            // match and the chevron sits past the D/E buttons. Generators are
            // remappable too (reshape on the per-instance graph), so this
            // unifies the surface — same chevron, same drawer.
            let author = self.context == CardContext::Author;
            let chevron_lane = if author {
                MAP_CHEVRON_W + DE_BUTTON_GAP
            } else {
                0.0
            };
            let slider_w =
                content_w - (DE_BUTTON_SIZE + DE_BUTTON_GAP) * 3.0 - chevron_lane;
            // Same growth rule as the effect card (see build_effect_sliders).
            let label_width = crate::slider::label_width_for_row(content_w);

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
                        label_id,
                        button_id,
                    });
                    self.toggle_cache[i] = on;
                    cy += ROW_HEIGHT + ROW_SPACING;
                } else {
                    // Slider row — shared per-param core. Generators parent rows
                    // flat to `-1`, use the gen-param slider palette, the
                    // body-size driver-config font, and always show the `E`
                    // button (generators always support envelopes).
                    let row_y = cy;
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
                        label_width,
                    );
                    self.slider_ids[i] = row.slider;
                    self.row_catcher_ids[i] = row.row_catcher;
                    self.trim_ids[i] = row.trim;
                    self.target_ids[i] = row.target;
                    self.envelope_config_ids[i] = row.envelope_config;
                    self.ableton_trim_ids[i] = row.ableton_trim;
                    self.audio_trim_ids[i] = row.audio_trim;
                    self.envelope_btn_ids[i] = row.envelope_btn;
                    self.driver_btn_ids[i] = row.driver_btn;
                    self.driver_config_ids[i] = row.driver_config;
                    self.ableton_config_ids[i] = row.ableton_config;
                    self.audio_btn_ids[i] = row.audio_btn;
                    self.audio_configs[i] = row.audio_config;
                    // Mapping-drawer chevron at the row's right edge (Author +
                    // mappable) — identical to the effect card. Opens the same
                    // sideways range/scale/offset/invert/curve drawer; click
                    // resolves via the shared `mapping_chevron_ids`.
                    if author && info.mappable {
                        let ch_x = cx + content_w - MAP_CHEVRON_W;
                        let ch_y = row_y + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
                        self.mapping_chevron_ids[i] = tree.add_button(
                            -1,
                            ch_x,
                            ch_y,
                            MAP_CHEVRON_W,
                            DE_BUTTON_SIZE,
                            UIStyle {
                                bg_color: Color32::TRANSPARENT,
                                hover_bg_color: color::HOVER_OVERLAY,
                                pressed_bg_color: color::PRESS_OVERLAY,
                                text_color: color::CHEVRON_COLOR,
                                font_size: FONT_SIZE,
                                text_align: TextAlign::Center,
                                corner_radius: 2.0,
                                ..UIStyle::default()
                            },
                            "\u{203A}", // ›
                        ) as i32;
                    }
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

    /// Re-pack the header badges + resize the name cell after the active-badge
    /// set changes in the sync path (no rebuild). Mirrors the packed layout
    /// `build_effect_header` computes, so a toggled badge lands flush-right and
    /// the name reclaims the freed width without a card rebuild.
    fn reposition_effect_badges(&self, tree: &mut UITree) {
        if self.toggle_btn_id < 0 || self.name_clip_id < 0 || self.mod_badge_bg_id < 0 {
            return;
        }
        let toggle_x = tree.get_bounds(self.toggle_btn_id as u32).x;
        let badge_y = tree.get_bounds(self.mod_badge_bg_id as u32).y;
        let badges = effect_badge_layout(
            toggle_x,
            self.state.has_graph_mod,
            self.state.has_abl,
            self.state.has_env,
            self.state.has_drv,
        );
        let park = toggle_x - GAP - BADGE_W;
        for (bg, txt, x) in [
            (self.mod_badge_bg_id, self.mod_badge_text_id, badges.mod_x),
            (self.abl_badge_bg_id, self.abl_badge_text_id, badges.abl_x),
            (self.env_badge_bg_id, self.env_badge_text_id, badges.env_x),
            (self.drv_badge_bg_id, self.drv_badge_text_id, badges.drv_x),
        ] {
            let r = Rect::new(x.unwrap_or(park), badge_y, BADGE_W, BADGE_H);
            if bg >= 0 {
                tree.set_bounds(bg as u32, r);
            }
            if txt >= 0 {
                tree.set_bounds(txt as u32, r);
            }
        }
        let name_b = tree.get_bounds(self.name_clip_id as u32);
        let name_w = (badges.name_right - name_b.x).max(10.0);
        tree.set_bounds(
            self.name_clip_id as u32,
            Rect::new(name_b.x, name_b.y, name_w, name_b.height),
        );
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
            // Re-pack the badges + resize the name cell now the active set changed.
            self.reposition_effect_badges(tree);
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
                        info.is_angle,
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
                        info.is_angle,
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
                && ids.label_id >= 0
                && ids.label_id as u32 == label_id
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

    /// The "A" audio-mod button action — always opens (arms) or closes
    /// (disarms) this param's audio drawer, never the Audio Setup modal. With no
    /// sends defined yet, arming auto-creates the project's first send and points
    /// this param at it in one undo step, so the drawer opens populated and ready
    /// (the user routes/renames sends in Audio Setup afterward). The drawer's own
    /// "+" adds further sends.
    fn audio_toggle_action(&self, target: GraphParamTarget, pi: usize) -> Vec<PanelAction> {
        let ms = &self.state.mod_state;
        if ms.audio_active.get(pi).copied().unwrap_or(false) {
            // Already armed → disarm (closes the drawer), regardless of sends.
            vec![PanelAction::AudioModToggle(target, self.pid_at(pi))]
        } else if ms.audio_send_ids.is_empty() {
            // Not armed, no send to point at → open Audio Setup so the user can
            // create one. Sends are defined there, never from the drawer.
            vec![PanelAction::OpenAudioSetup]
        } else {
            // Not armed, sends exist → arm at the project's first send.
            vec![PanelAction::AudioModToggle(target, self.pid_at(pi))]
        }
    }

    /// Build an `AudioModSetSource` action for a param, combining the current
    /// send + feature selection (from `mod_state`) with the one dimension the
    /// click changed. Empty when no send resolves (nothing to point at).
    /// Build an `AudioModSetSource` from the param's current selections, with one
    /// axis optionally overridden (the clicked send / feature-kind / band).
    fn audio_set_source_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        send_override: Option<usize>,
        kind_override: Option<usize>,
        band_override: Option<usize>,
    ) -> Vec<PanelAction> {
        use super::param_slider_shared::{audio_band_from_index, audio_kind_from_index};
        let ms = &self.state.mod_state;
        let send_k = send_override
            .map(|k| k as i32)
            .unwrap_or_else(|| ms.audio_send_idx.get(pi).copied().unwrap_or(-1));
        let Some(send_id) = (send_k >= 0)
            .then(|| ms.audio_send_ids.get(send_k as usize).cloned())
            .flatten()
        else {
            return vec![];
        };
        let kind_idx =
            kind_override.unwrap_or_else(|| ms.audio_kind_idx.get(pi).copied().unwrap_or(0) as usize);
        let band_idx =
            band_override.unwrap_or_else(|| ms.audio_band_idx.get(pi).copied().unwrap_or(0) as usize);
        let feature = manifold_core::AudioFeature::new(
            audio_kind_from_index(kind_idx),
            audio_band_from_index(band_idx),
        );
        vec![PanelAction::AudioModSetSource(target, self.pid_at(pi), send_id, feature)]
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

        // Mapping-drawer chevron (Author context) → open the sideways
        // range/scale/offset/invert/curve drawer for this row's binding.
        if let Some(pi) = self
            .mapping_chevron_ids
            .iter()
            .position(|&cid| cid >= 0 && cid == id)
        {
            return vec![PanelAction::OpenCardMapping(self.pid_at(pi))];
        }

        // Per-param row elements (D/E buttons, config drawers, label copy) —
        // shared dispatch; map the abstract RowClick to effect-side actions.
        if let Some(rc) = match_param_row_click(
            id,
            &self.driver_btn_ids,
            &self.envelope_btn_ids,
            &self.driver_config_ids,
            &self.ableton_config_ids,
            &self.audio_btn_ids,
            &self.audio_configs,
            &self.slider_ids,
            &self.osc_addresses,
            &self.param_info,
        ) {
            return match rc {
                RowClick::DriverToggle(pi) => {
                    vec![PanelAction::DriverToggle(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::EnvelopeToggle(pi) => {
                    vec![PanelAction::EnvelopeToggle(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::DriverConfig(pi, action) => {
                    vec![PanelAction::DriverConfig(GraphParamTarget::Effect(ei), self.pid_at(pi), action)]
                }
                RowClick::AbletonInvert(pi) => {
                    vec![PanelAction::AbletonInvertToggle(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::AudioToggle(pi) => {
                    self.audio_toggle_action(GraphParamTarget::Effect(ei), pi)
                }
                RowClick::AudioSelectSend(pi, k) => {
                    self.audio_set_source_action(GraphParamTarget::Effect(ei), pi, Some(k), None, None)
                }
                RowClick::AudioSelectKind(pi, k) => {
                    self.audio_set_source_action(GraphParamTarget::Effect(ei), pi, None, Some(k), None)
                }
                RowClick::AudioSelectBand(pi, b) => {
                    self.audio_set_source_action(GraphParamTarget::Effect(ei), pi, None, None, Some(b))
                }
                RowClick::AudioToggleInvert(pi) => {
                    vec![PanelAction::AudioModSetInvert(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::AudioToggleRate(pi) => {
                    vec![PanelAction::AudioModSetRateOfChange(
                        GraphParamTarget::Effect(ei),
                        self.pid_at(pi),
                    )]
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

        // Mapping-drawer chevron (Author context) → open the sideways
        // range/scale/offset/invert/curve drawer for this row's param. Same
        // action the effect card emits; the host resolves it against the
        // watched generator target (the unified mapping surface).
        if let Some(pi) = self
            .mapping_chevron_ids
            .iter()
            .position(|&cid| cid >= 0 && cid == id)
        {
            return vec![PanelAction::OpenCardMapping(self.pid_at(pi))];
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
            &self.ableton_config_ids,
            &self.audio_btn_ids,
            &self.audio_configs,
            &self.slider_ids,
            &self.osc_addresses,
            &self.param_info,
        ) {
            return match rc {
                RowClick::DriverToggle(pi) => vec![PanelAction::DriverToggle(GraphParamTarget::Generator, self.pid_at(pi))],
                RowClick::EnvelopeToggle(pi) => {
                    vec![PanelAction::EnvelopeToggle(GraphParamTarget::Generator, self.pid_at(pi))]
                }
                RowClick::DriverConfig(pi, action) => {
                    vec![PanelAction::DriverConfig(GraphParamTarget::Generator, self.pid_at(pi), action)]
                }
                RowClick::AbletonInvert(pi) => {
                    vec![PanelAction::AbletonInvertToggle(GraphParamTarget::Generator, self.pid_at(pi))]
                }
                RowClick::AudioToggle(pi) => {
                    self.audio_toggle_action(GraphParamTarget::Generator, pi)
                }
                RowClick::AudioSelectSend(pi, k) => {
                    self.audio_set_source_action(GraphParamTarget::Generator, pi, Some(k), None, None)
                }
                RowClick::AudioSelectKind(pi, k) => {
                    self.audio_set_source_action(GraphParamTarget::Generator, pi, None, Some(k), None)
                }
                RowClick::AudioSelectBand(pi, b) => {
                    self.audio_set_source_action(GraphParamTarget::Generator, pi, None, None, Some(b))
                }
                RowClick::AudioToggleInvert(pi) => {
                    vec![PanelAction::AudioModSetInvert(GraphParamTarget::Generator, self.pid_at(pi))]
                }
                RowClick::AudioToggleRate(pi) => {
                    vec![PanelAction::AudioModSetRateOfChange(
                        GraphParamTarget::Generator,
                        self.pid_at(pi),
                    )]
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
                && t.label_id >= 0
                && id == t.label_id
                && let Some(addr) = self.osc_addresses.get(pi).and_then(|a| a.clone())
            {
                self.copied_flash.trigger(t.label_id as u32);
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

    /// The trim-handle node ids for a modulator kind. The three kinds keep
    /// separate id vectors (they overlay the same track simultaneously), and
    /// this is the one place a `TrimKind` selects between them.
    fn trim_ids_for(&self, kind: TrimKind) -> &[Option<TrimHandleIds>] {
        match kind {
            TrimKind::Driver => &self.trim_ids,
            TrimKind::Ableton => &self.ableton_trim_ids,
            TrimKind::Audio => &self.audio_trim_ids,
        }
    }

    /// The current `[min, max]` output sub-range a kind's trim handles
    /// represent at param index `pi`. Driver/audio read card `mod_state` and
    /// default to the full range; Ableton reads the mapping range on
    /// `param_info` and returns `None` when the param has no mapping — the old
    /// `ableton_range` guard, preserved so an Ableton drag can't proceed
    /// without a mapping to edit.
    fn trim_range(&self, kind: TrimKind, pi: usize) -> Option<(f32, f32)> {
        match kind {
            TrimKind::Driver => Some((
                self.state.mod_state.trim_min.get(pi).copied().unwrap_or(0.0),
                self.state.mod_state.trim_max.get(pi).copied().unwrap_or(1.0),
            )),
            TrimKind::Ableton => self.param_info[pi].ableton_range,
            TrimKind::Audio => Some((
                self.state
                    .mod_state
                    .audio_range_min
                    .get(pi)
                    .copied()
                    .unwrap_or(0.0),
                self.state
                    .mod_state
                    .audio_range_max
                    .get(pi)
                    .copied()
                    .unwrap_or(1.0),
            )),
        }
    }

    /// Write a kind's live trim range at param index `pi` back to its card-side
    /// store during a drag. The mirror of [`trim_range`].
    fn set_trim_range(&mut self, kind: TrimKind, pi: usize, min: f32, max: f32) {
        match kind {
            TrimKind::Driver => {
                if let Some(v) = self.state.mod_state.trim_min.get_mut(pi) {
                    *v = min;
                }
                if let Some(v) = self.state.mod_state.trim_max.get_mut(pi) {
                    *v = max;
                }
            }
            TrimKind::Ableton => {
                self.param_info[pi].ableton_range = Some((min, max));
            }
            TrimKind::Audio => {
                if let Some(v) = self.state.mod_state.audio_range_min.get_mut(pi) {
                    *v = min;
                }
                if let Some(v) = self.state.mod_state.audio_range_max.get_mut(pi) {
                    *v = max;
                }
            }
        }
    }

    /// Unified pointer-down hit-testing for both card kinds. Steps 1-4 grab the
    /// modulation widgets (the envelope target handle, the envelope decay slider,
    /// driver/Ableton/audio trim bars); step 5 is the param slider, with the
    /// proximity catch-zones for the target handle and driver trim handles. The
    /// emitted target comes from `param_target()`, so effect and generator share
    /// one path; toggle/trigger rows (generator-only, no slider widget) are
    /// skipped in step 5.
    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        let target = self.param_target();

        // 1. Envelope target handle (the orange grab bar on the slider track).
        for (pi, etarget) in self.target_ids.iter().enumerate() {
            if let Some(t) = etarget
                && node_id as i32 == t.target_bar_id
            {
                self.drag.dragging_target_param = pi as i32;
                return vec![PanelAction::TargetSnapshot(target, self.pid_at(pi))];
            }
        }

        // 2. Envelope decay slider (in the drawer).
        for (pi, env_cfg) in self.envelope_config_ids.iter().enumerate() {
            if let Some(c) = env_cfg
                && node_id == c.decay_slider.track
            {
                self.drag.dragging_decay_param = pi as i32;
                let norm = BitmapSlider::x_to_normalized(c.decay_slider.track_rect, pos.x);
                let decay = norm.clamp(0.0, 1.0) * ENV_DECAY_MAX;
                return vec![
                    PanelAction::EnvDecaySnapshot(target, self.pid_at(pi)),
                    PanelAction::EnvDecayChanged(target, self.pid_at(pi), decay),
                ];
            }
        }

        // 2b. Audio shaping sliders (Amount/Attack/Release) in the drawer share
        // one drag path; the slider index picks which AudioModShape scalar it
        // edits. Snapshot for undo, then dispatch the first live value.
        for (pi, audio_cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, _)) = audio_cfg else { continue };
            for (si, which) in [
                (0usize, AudioShapeParam::Sensitivity),
                (1, AudioShapeParam::Attack),
                (2, AudioShapeParam::Release),
            ] {
                if let Some(sl) = dids.sliders.get(si)
                    && node_id == sl.track
                {
                    let norm = BitmapSlider::x_to_normalized(sl.track_rect, pos.x).clamp(0.0, 1.0);
                    let value = audio_shape_value_from_norm(which, norm);
                    self.drag.dragging_audio_shape = Some((pi, which));
                    let pid = self.pid_at(pi);
                    return vec![
                        PanelAction::AudioModShapeSnapshot(target, pid.clone()),
                        PanelAction::AudioModShapeParamChanged(target, pid, which, value),
                    ];
                }
            }
        }

        // 3. Trim bars — driver, Ableton, and audio share one hit-test;
        // `TrimKind` records which modulator's range was grabbed. The probe
        // order (driver → Ableton → audio) matches the old three-loop order.
        let mut trim_hit: Option<(TrimKind, usize, bool)> = None;
        'trim: for kind in [TrimKind::Driver, TrimKind::Ableton, TrimKind::Audio] {
            for (pi, trim) in self.trim_ids_for(kind).iter().enumerate() {
                if let Some(t) = trim {
                    if node_id as i32 == t.min_bar_id {
                        trim_hit = Some((kind, pi, true));
                        break 'trim;
                    }
                    if node_id as i32 == t.max_bar_id {
                        trim_hit = Some((kind, pi, false));
                        break 'trim;
                    }
                }
            }
        }
        if let Some((kind, pi, is_min)) = trim_hit {
            self.drag.dragging_trim = Some((kind, pi, is_min));
            return vec![PanelAction::TrimSnapshot(kind, target, self.pid_at(pi))];
        }

        // 5. Param slider tracks. Toggle/trigger rows have no slider widget, so
        // skip them. When a driver is expanded, the thin (4px) trim bars get an
        // ~8px proximity catch-zone so they're grabbable by feel before falling
        // through to a normal param drag.
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
                && (node_id == ids.track || {
                    // Also accept clicks on the driver trim bar / fill / target
                    // handle nodes that overlay this track.
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
                        self.drag.dragging_trim = Some((TrimKind::Driver, pi, true));
                        let _ = trim;
                        return vec![PanelAction::TrimSnapshot(TrimKind::Driver, target, self.pid_at(pi))];
                    }
                    if dist_max < hit_zone {
                        self.drag.dragging_trim = Some((TrimKind::Driver, pi, false));
                        return vec![PanelAction::TrimSnapshot(TrimKind::Driver, target, self.pid_at(pi))];
                    }
                }

                // If the envelope is armed, the orange target handle gets an ~8px
                // proximity catch-zone so it's grabbable by feel on the track.
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
                    let tgt = self
                        .state
                        .mod_state
                        .target_norm
                        .get(pi)
                        .copied()
                        .unwrap_or(1.0);
                    let target_center = base_x + tgt * usable;
                    if (pos.x - target_center).abs() < 8.0 {
                        self.drag.dragging_target_param = pi as i32;
                        return vec![PanelAction::TargetSnapshot(target, self.pid_at(pi))];
                    }
                }

                // No trim/target handle nearby — normal param slider drag
                self.drag.dragging_param = pi as i32;
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos.x);
                let info = &self.param_info[pi];
                let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                let val = if info.whole_numbers { val.round() } else { val };
                return vec![
                    PanelAction::ParamSnapshot(target, self.pid_at(pi)),
                    PanelAction::ParamChanged(target, self.pid_at(pi), val),
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

        // Envelope target handle drag — update depth, reposition the orange bar
        // along the parameter's own track, dispatch the Target change.
        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            if let Some(slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(slider.track_rect, pos.x);
                if let Some(v) = self.state.mod_state.target_norm.get_mut(pi) {
                    *v = norm;
                }
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
                    ParamCardKind::Effect => vec![PanelAction::TargetChanged(GraphParamTarget::Effect(ei), pid, norm)],
                    ParamCardKind::Generator => vec![PanelAction::TargetChanged(GraphParamTarget::Generator, pid, norm)],
                };
            }
        }

        // Envelope decay slider drag — update the drawer slider's fill + value,
        // dispatch the decay change (in beats).
        if self.drag.dragging_decay_param >= 0 {
            let pi = self.drag.dragging_decay_param as usize;
            if let Some(cfg) = self.envelope_config_ids.get(pi).and_then(|c| c.as_ref()) {
                let norm = BitmapSlider::x_to_normalized(cfg.decay_slider.track_rect, pos.x)
                    .clamp(0.0, 1.0);
                let decay = norm * ENV_DECAY_MAX;
                if let Some(v) = self.state.mod_state.env_decay.get_mut(pi) {
                    *v = decay;
                }
                BitmapSlider::update_value(tree, &cfg.decay_slider, norm, &format!("{decay:.2}"));
                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::EnvDecayChanged(GraphParamTarget::Effect(ei), pid, decay)],
                    ParamCardKind::Generator => vec![PanelAction::EnvDecayChanged(GraphParamTarget::Generator, pid, decay)],
                };
            }
        }

        // Audio shaping slider drag — update fill + value, dispatch live edit.
        if let Some((pi, which)) = self.drag.dragging_audio_shape {
            let si = match which {
                AudioShapeParam::Sensitivity => 0,
                AudioShapeParam::Attack => 1,
                AudioShapeParam::Release => 2,
            };
            let rect = self
                .audio_configs
                .get(pi)
                .and_then(|c| c.as_ref())
                .and_then(|(d, _)| d.sliders.get(si))
                .map(|sl| sl.track_rect);
            if let Some(rect) = rect {
                let norm = BitmapSlider::x_to_normalized(rect, pos.x).clamp(0.0, 1.0);
                let value = audio_shape_value_from_norm(which, norm);
                match which {
                    AudioShapeParam::Sensitivity => {
                        if let Some(v) = self.state.mod_state.audio_sensitivity.get_mut(pi) {
                            *v = value;
                        }
                    }
                    AudioShapeParam::Attack => {
                        if let Some(v) = self.state.mod_state.audio_attack_ms.get_mut(pi) {
                            *v = value;
                        }
                    }
                    AudioShapeParam::Release => {
                        if let Some(v) = self.state.mod_state.audio_release_ms.get_mut(pi) {
                            *v = value;
                        }
                    }
                }
                let text = audio_shape_value_text(which, value);
                if let Some((d, _)) = self.audio_configs.get(pi).and_then(|c| c.as_ref())
                    && let Some(sl) = d.sliders.get(si)
                {
                    BitmapSlider::update_value(tree, sl, norm, &text);
                }
                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::AudioModShapeParamChanged(
                        GraphParamTarget::Effect(ei),
                        pid,
                        which,
                        value,
                    )],
                    ParamCardKind::Generator => vec![PanelAction::AudioModShapeParamChanged(
                        GraphParamTarget::Generator,
                        pid,
                        which,
                        value,
                    )],
                };
            }
        }

        // Trim bar drag (driver / Ableton / audio) — one path. Read the kind's
        // current range, clamp the dragged edge, write it back, reposition the
        // bars, emit the change. The clamp and `reposition_trim_bars` are
        // identical across kinds (`x_to_normalized` pre-clamps to [0,1], so the
        // old `norm.min`/`norm.clamp` spellings coincide); only the backing
        // store differs, and `TrimKind` selects it via the trim accessors.
        if let Some((kind, pi, is_min)) = self.drag.dragging_trim
            && let Some(track_rect) = self
                .slider_ids
                .get(pi)
                .and_then(|s| s.as_ref())
                .map(|s| s.track_rect)
            && let Some((cur_min, cur_max)) = self.trim_range(kind, pi)
        {
            let norm = BitmapSlider::x_to_normalized(track_rect, pos.x);
            let (new_min, new_max) = if is_min {
                (norm.min(cur_max), cur_max)
            } else {
                (cur_min, norm.max(cur_min))
            };
            self.set_trim_range(kind, pi, new_min, new_max);

            // Visual update: reposition this kind's trim bar nodes in the tree.
            if let Some(t) = self.trim_ids_for(kind).get(pi).and_then(|t| t.as_ref()).copied() {
                super::param_slider_shared::reposition_trim_bars(
                    tree, track_rect, &t, new_min, new_max,
                );
            }

            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => {
                    vec![PanelAction::TrimChanged(kind, GraphParamTarget::Effect(ei), pid, new_min, new_max)]
                }
                ParamCardKind::Generator => {
                    vec![PanelAction::TrimChanged(kind, GraphParamTarget::Generator, pid, new_min, new_max)]
                }
            };
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
                    info.is_angle,
                    info.value_labels.as_deref(),
                );
                BitmapSlider::update_value(tree, ids, display_norm, &text);
                self.param_cache[pi] = val;
                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::ParamChanged(GraphParamTarget::Effect(ei), pid, val)],
                    ParamCardKind::Generator => vec![PanelAction::ParamChanged(GraphParamTarget::Generator, pid, val)],
                };
            }
        }

        Vec::new()
    }

    /// Drag-end dispatch — commit the active drag. Identical bookkeeping for
    /// both kinds; only the emitted [`PanelAction`] variant differs.
    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        if self.drag.dragging_target_param >= 0 {
            let pi = self.drag.dragging_target_param as usize;
            self.drag.dragging_target_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::TargetCommit(GraphParamTarget::Effect(ei), pid)],
                ParamCardKind::Generator => vec![PanelAction::TargetCommit(GraphParamTarget::Generator, pid)],
            };
        }
        if self.drag.dragging_decay_param >= 0 {
            let pi = self.drag.dragging_decay_param as usize;
            self.drag.dragging_decay_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::EnvDecayCommit(GraphParamTarget::Effect(ei), pid)],
                ParamCardKind::Generator => vec![PanelAction::EnvDecayCommit(GraphParamTarget::Generator, pid)],
            };
        }
        if let Some((pi, _)) = self.drag.dragging_audio_shape.take() {
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::AudioModShapeCommit(GraphParamTarget::Effect(ei), pid)],
                ParamCardKind::Generator => vec![PanelAction::AudioModShapeCommit(GraphParamTarget::Generator, pid)],
            };
        }
        if let Some((kind, pi, _)) = self.drag.dragging_trim.take() {
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::TrimCommit(kind, GraphParamTarget::Effect(ei), pid)],
                ParamCardKind::Generator => vec![PanelAction::TrimCommit(kind, GraphParamTarget::Generator, pid)],
            };
        }
        if self.drag.dragging_param >= 0 {
            let pi = self.drag.dragging_param as usize;
            self.drag.dragging_param = -1;
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::ParamCommit(GraphParamTarget::Effect(ei), pid)],
                ParamCardKind::Generator => vec![PanelAction::ParamCommit(GraphParamTarget::Generator, pid)],
            };
        }

        Vec::new()
    }

    /// Node-intent dispatch for this card's right-click gestures. The sole
    /// right-click path for both the inspector and the graph-editor card.
    /// Declarative intent + fold-up: specific intents on the slider track
    /// (reset) and label (perform mapping) win, and the card root claims its
    /// whole area so a right-click on any dead zone — slider fill/thumb/value
    /// cell, row gaps, padding — folds up to the card context menu instead of
    /// being silently swallowed. See `docs/NODE_INTENT_DISPATCH.md`.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        use crate::intent::Gesture::RightClick;

        let target = match self.kind {
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_index),
            ParamCardKind::Generator => GraphParamTarget::Generator,
        };

        // Card root: claim the whole area + the context-menu action. Any
        // descendant without a more specific intent folds here.
        if self.border_id >= 0 {
            intents.claim_area(self.border_id as u32);
            intents.on(self.border_id as u32, RightClick, PanelAction::CardRightClicked(target));
        }

        // Per-param specific intents.
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            // Generator toggle/trigger rows have no reset/map gesture — they
            // fall through to the card claim like any other dead zone.
            if matches!(self.kind, ParamCardKind::Generator)
                && self
                    .param_info
                    .get(pi)
                    .map(|i| i.is_toggle || i.is_trigger)
                    .unwrap_or(false)
            {
                continue;
            }
            let Some(ids) = slider else { continue };

            // Slider track → reset to default.
            let default = self.param_info.get(pi).map(|i| i.default).unwrap_or(0.0);
            intents.on(ids.track, RightClick, PanelAction::ParamRightClick(target, self.pid_at(pi), default));

            // Rest of the row → perform-mapping menu (Perform context only;
            // Author uses the right-edge mapping drawer instead). Registered on
            // both the interactive label and the full-row catcher behind the
            // value cell + gaps, so a right-click anywhere on the row that isn't
            // the track reliably opens the param menu — no narrow-target lottery.
            if self.context == CardContext::Perform {
                let menu = PanelAction::ParamLabelRightClick(target, self.pid_at(pi));
                if ids.label >= 0 {
                    intents.on(ids.label as u32, RightClick, menu.clone());
                }
                if let Some(catcher) = self.row_catcher_ids.get(pi).copied()
                    && catcher >= 0
                {
                    intents.claim_area(catcher as u32);
                    intents.on(catcher as u32, RightClick, menu);
                }
            }
        }
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
                    is_angle: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("strength"),
                    name: "Strength".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    whole_numbers: false,
                    is_angle: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
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
            env_decay: vec![1.0; n],
            driver_beat_div_idx: vec![-1; n],
            driver_waveform_idx: vec![-1; n],
            driver_reversed: vec![false; n],
            driver_dotted: vec![false; n],
            driver_triplet: vec![false; n],
            audio: Default::default(),
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

    /// Config with the second param marked mappable (a user-tail binding).
    fn effect_config_with_mappable() -> ParamCardConfig {
        let mut c = effect_config();
        c.params[1].mappable = true;
        c
    }

    #[test]
    fn author_context_suppresses_perform_chrome() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.set_context(CardContext::Author);
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        // Cog ("open graph editor") and the drag-reorder handle are gone.
        assert!(panel.cog_btn_id < 0, "cog suppressed in Author");
        assert!(panel.drag_icon_id < 0, "drag handle suppressed in Author");
        // Mapping chevron only on the mappable row.
        assert!(panel.mapping_chevron_ids[0] < 0, "row 0 not mappable");
        assert!(panel.mapping_chevron_ids[1] >= 0, "row 1 mappable → chevron");
    }

    #[test]
    fn perform_context_has_no_mapping_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new(); // default Perform
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        // Perform keeps the cog and never draws the mapping chevron.
        assert!(panel.cog_btn_id >= 0, "cog present in Perform");
        assert!(panel.mapping_chevron_ids.iter().all(|&id| id < 0));
    }

    #[test]
    fn mapping_chevron_click_emits_open_card_mapping() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.set_context(CardContext::Author);
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        let chevron = panel.mapping_chevron_ids[1];
        assert!(chevron >= 0);
        let actions = panel.handle_click(chevron as u32);
        assert!(
            matches!(&actions[..], [PanelAction::OpenCardMapping(pid)] if pid == "strength"),
            "got {actions:?}"
        );
        // The chevron also has a resolvable anchor rect by binding id.
        assert!(panel.mapping_chevron_rect(&tree, "strength").is_some());
        assert!(panel.mapping_chevron_rect(&tree, "radius").is_none());
    }

    /// Generator config with the second param marked mappable — generators are
    /// remappable too, so the Author-context mapping chevron must appear, same
    /// as effects (the unified surface).
    fn generator_config_with_mappable() -> ParamCardConfig {
        let mut c = effect_config();
        c.kind = ParamCardKind::Generator;
        c.params[1].mappable = true;
        c
    }

    #[test]
    fn generator_author_context_shows_mapping_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.set_context(CardContext::Author);
        panel.configure(&generator_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        // Same as the effect card: chevron only on the mappable row, click opens
        // the card mapping drawer, and the anchor rect resolves by binding id.
        assert!(panel.mapping_chevron_ids[0] < 0, "row 0 not mappable");
        let chevron = panel.mapping_chevron_ids[1];
        assert!(chevron >= 0, "generator mappable row → chevron");
        let actions = panel.handle_click(chevron as u32);
        assert!(
            matches!(&actions[..], [PanelAction::OpenCardMapping(pid)] if pid == "strength"),
            "got {actions:?}"
        );
        assert!(panel.mapping_chevron_rect(&tree, "strength").is_some());
    }

    #[test]
    fn generator_perform_context_has_no_mapping_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new(); // default Perform
        panel.configure(&generator_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));
        assert!(panel.mapping_chevron_ids.iter().all(|&id| id < 0));
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
            PanelAction::DriverToggle(GraphParamTarget::Effect(ei), param_id) => {
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

        // Arming the envelope adds the orange target handle on the slider track.
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
                    is_angle: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("invert"),
                    name: "Invert".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    whole_numbers: false,
                    is_angle: false,
                    exposed: true,
                    is_toggle: true,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
                ParamInfo {
                    param_id: std::borrow::Cow::Borrowed("scale"),
                    name: "Scale".into(),
                    min: 0.1,
                    max: 5.0,
                    default: 1.0,
                    whole_numbers: false,
                    is_angle: false,
                    exposed: true,
                    is_toggle: false,
                    is_trigger: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
            ],
            string_params: vec![],
            driver_active: vec![false; 3],
            envelope_active: vec![false; 3],
            trim_min: vec![0.0; 3],
            trim_max: vec![1.0; 3],
            target_norm: vec![1.0; 3],
            env_decay: vec![1.0; 3],
            driver_beat_div_idx: vec![-1; 3],
            driver_waveform_idx: vec![-1; 3],
            driver_reversed: vec![false; 3],
            driver_dotted: vec![false; 3],
            driver_triplet: vec![false; 3],
            audio: Default::default(),
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

    #[test]
    fn gen_compute_height_with_audio_drawer_expanded() {
        // Arming an audio mod auto-shows the per-param audio drawer; the card
        // must reserve its height or the drawer draws past the card bounds.
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());

        let base_h = panel.compute_height();
        panel.state.mod_state.audio_active[0] = true;
        let expanded_h = panel.compute_height();

        assert!(expanded_h > base_h);
        let audio_h = crate::panels::param_slider_shared::audio_config_height();
        assert!((expanded_h - base_h - audio_h).abs() < 0.1);
    }
}
