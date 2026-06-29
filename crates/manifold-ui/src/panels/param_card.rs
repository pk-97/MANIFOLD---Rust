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
use crate::chrome::{Align, ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;
use manifold_foundation::{EffectId, LayerId};

// Stable keys for the host-owned card frame + header.
const KEY_BORDER: u64 = 90_001;
const KEY_INNER: u64 = 90_002;
const KEY_HEADER_BG: u64 = 90_003;
const KEY_NAME: u64 = 90_004;
const KEY_CHEVRON: u64 = 90_005;
const KEY_COG: u64 = 90_006;
const KEY_CHANGE: u64 = 90_007;
const KEY_DRAG: u64 = 90_008;
const KEY_NAME_CLIP: u64 = 90_009;
const KEY_TOGGLE: u64 = 90_010;

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
    /// Stable [`ParamId`](manifold_foundation::ParamId) for this slot — for
    /// static-tier params the `&'static str` declared in the preset's
    /// `ParamSpec`; for user-tier (graph-editor-exposed) effect params the
    /// owned id from `PresetInstance.user_param_bindings[j].id`. Carried on
    /// the wire when a widget emits a [`PanelAction`](super::PanelAction) so
    /// the bridge never does a positional `pi → ParamId` lookup.
    pub param_id: manifold_foundation::ParamId,
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
    /// Per-param driver free-running period in beats (`Some` => free mode).
    pub driver_free_period: Vec<Option<f32>>,
    /// Audio-modulation state (per-param active/send/feature + card-level send
    /// list). Bundled so the config grows by one field.
    pub audio: super::param_slider_shared::AudioCardState,
}

// ── Layout constants ─────────────────────────────────────────────
//
// Shared between both card kinds. The shell furniture each kind draws on top
// (effect: drag-handle + ABL/ENV/DRV/MOD badges + ON/OFF toggle; generator:
// Change button) carries its own kind-specific widths.

const HEADER_HEIGHT: f32 = color::HEADER_ROW_HEIGHT; // §14.2 rule 5: one header height
/// Breathing room between the coloured header and the first param row, so the
/// slider doesn't butt against the header. Matches the card's bottom padding.
const HEADER_BODY_GAP: f32 = PADDING;
const BORDER_W: f32 = 1.0;
// Card corner = the design-token card radius (Phase 3). Radius is purely
// visual — it doesn't move any laid rect, so the golden header-layout tests
// are unaffected.
const CORNER_RADIUS: f32 = color::CARD_RADIUS;
// §14.5 E — the inter-card gap is owned by the container (`inspector::SECTION_GAP`),
// not split between margin + gap. Zero here; the card reports just its frame height.
const CARD_BOTTOM_MARGIN: f32 = 0.0;
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

// Generator shell furniture. A toggle/trigger row stands in for a value, so its
// button is the same width as the slider value box and right-aligns to the same
// column — the right edge of every row lines up.
const TOGGLE_BTN_W: f32 = crate::slider::VALUE_BOX_W;
const TOGGLE_BTN_H: f32 = 16.0;
const CHANGE_BTN_W: f32 = 60.0;
const CHANGE_BTN_H: f32 = 16.0;

// ── Internal node ID structs ─────────────────────────────────────

/// Generator toggle/trigger row node IDs (button + its label).
struct ToggleParamIds {
    label_id: Option<NodeId>,
    button_id: NodeId,
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
    aud_x: Option<f32>,
    name_right: f32,
}

/// Lay the visible header badges out CENTERED in the region between the name's
/// left edge (`content_left`) and the toggle. Packing only the active ones keeps
/// the cluster tight; centering keeps it clear of the ON/OFF toggle so the
/// badges don't read as another button. The name cell clips to `name_right`
/// (just before the cluster's left edge).
fn effect_badge_layout(
    content_left: f32,
    toggle_x: f32,
    show_mod: bool,
    show_abl: bool,
    show_env: bool,
    show_drv: bool,
    show_aud: bool,
) -> BadgeLayout {
    let shows = [show_mod, show_abl, show_env, show_drv, show_aud];
    let count = shows.iter().filter(|s| **s).count();
    let region_left = content_left;
    let region_right = toggle_x - GAP;
    let block_w = if count == 0 {
        0.0
    } else {
        count as f32 * BADGE_W + (count as f32 - 1.0) * GAP
    };
    // Centre the block in the region; clamp so it never runs under the toggle nor
    // off the left edge.
    let centered = (region_left + region_right) * 0.5 - block_w * 0.5;
    let block_left = centered.clamp(region_left, (region_right - block_w).max(region_left));
    let mut xs: [Option<f32>; 5] = [None; 5];
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
        aud_x: xs[4],
        name_right: if count == 0 { region_right } else { block_left - GAP },
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
    /// Aggregate: any param has an armed audio modulation (AUD badge).
    pub has_audio: bool,
    /// The card's graph diverges from the catalog default (MOD badge only).
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
            has_audio: false,
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

    /// Host for the declarative card frame (border + inner bg). The header and
    /// param rows are still built imperatively into the host-laid inner bg
    /// (staged migration — see `frame_view`).
    host: ChromeHost,

    // ── Node IDs — card shell (shared) ──
    border_id: Option<NodeId>,
    inner_bg_id: Option<NodeId>,
    header_bg_id: Option<NodeId>,
    name_label_id: Option<NodeId>,
    /// Transparent CLIPS_CHILDREN container sized to the name cell. The name
    /// label nests inside it so a long effect name is clipped at the cell edge
    /// instead of overflowing into the header badges. Resized live in sync when
    /// the active-badge set changes.
    name_clip_id: Option<NodeId>,
    chevron_btn_id: Option<NodeId>,
    cog_btn_id: Option<NodeId>,

    // ── Node IDs — effect shell ──
    drag_icon_id: Option<NodeId>,
    toggle_btn_id: Option<NodeId>,
    abl_badge_bg_id: Option<NodeId>,
    abl_badge_text_id: Option<NodeId>,
    env_badge_bg_id: Option<NodeId>,
    env_badge_text_id: Option<NodeId>,
    drv_badge_bg_id: Option<NodeId>,
    drv_badge_text_id: Option<NodeId>,
    mod_badge_bg_id: Option<NodeId>,
    mod_badge_text_id: Option<NodeId>,
    aud_badge_bg_id: Option<NodeId>,
    aud_badge_text_id: Option<NodeId>,

    // ── Node IDs — generator shell ──
    change_btn_id: Option<NodeId>,

    // ── Dirty-check cache (effect badges + enabled) ──
    cached_enabled: bool,
    cached_has_env: bool,
    cached_has_drv: bool,
    cached_has_abl: bool,
    cached_has_audio: bool,
    cached_has_graph_mod: bool,

    // ── Node IDs — per-param (shared) ──
    slider_ids: Vec<Option<SliderNodeIds>>,
    /// Per-param base (pre-modulation) value, cached each sync so a value-cell
    /// double-click prefills the type-in box with the user-set value, not the
    /// live modulated display. Sized to the param count in `configure`.
    base_values: Vec<f32>,
    /// Per-param transparent full-row hit catcher behind the slider widgets.
    /// Carries the param's right-click menu intent so the value cell + gaps
    /// resolve to the param menu (track stays instant-reset). None if unbuilt.
    row_catcher_ids: Vec<Option<NodeId>>,
    driver_btn_ids: Vec<Option<NodeId>>,
    envelope_btn_ids: Vec<Option<NodeId>>,
    driver_config_ids: Vec<Option<DriverConfigIds>>,
    /// Per-param "A" audio-mod button node id.
    audio_btn_ids: Vec<Option<NodeId>>,
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
    /// Per-param stored modulation-config tab choice — which config the shared
    /// drawer shows when ≥2 are active. UI-only state, preserved across rebuilds
    /// (configure resizes without clobbering existing entries). `resolve_active_tab`
    /// clamps a stale choice to an active one at build time.
    mod_active_tab: Vec<ModTab>,
    /// Per-param modulation-config tab strip node ids paired with their `ModTab`,
    /// for routing tab clicks. Empty for rows with fewer than two active configs.
    /// Rebuilt each frame.
    mod_tab_ids: Vec<Vec<(NodeId, ModTab)>>,
    /// §6b compact mode — when true, every modulation config drawer on this card
    /// is hidden (mods stay armed; arm buttons + slider track overlays still
    /// show). Driven globally by the inspector's "hide mod settings" toggle.
    compact: bool,

    // ── Node IDs — per-param (generator) ──
    toggle_ids: Vec<Option<ToggleParamIds>>,
    string_param_btn_ids: Vec<Option<NodeId>>,

    /// Per-param sideways-mapping-drawer chevron (Author context, mappable rows
    /// only). `None` for rows without one. Indexed by param index.
    mapping_chevron_ids: Vec<Option<NodeId>>,

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
            host: ChromeHost::new(),
            border_id: None,
            inner_bg_id: None,
            header_bg_id: None,
            name_label_id: None,
            name_clip_id: None,
            chevron_btn_id: None,
            cog_btn_id: None,
            drag_icon_id: None,
            toggle_btn_id: None,
            abl_badge_bg_id: None,
            abl_badge_text_id: None,
            env_badge_bg_id: None,
            env_badge_text_id: None,
            drv_badge_bg_id: None,
            drv_badge_text_id: None,
            mod_badge_bg_id: None,
            mod_badge_text_id: None,
            aud_badge_bg_id: None,
            aud_badge_text_id: None,
            change_btn_id: None,
            cached_enabled: true,
            cached_has_env: false,
            cached_has_drv: false,
            cached_has_abl: false,
            cached_has_audio: false,
            cached_has_graph_mod: false,
            slider_ids: Vec::new(),
            base_values: Vec::new(),
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
            mod_active_tab: Vec::new(),
            mod_tab_ids: Vec::new(),
            compact: false,
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
            &config.driver_free_period,
        );
        self.state.mod_state.sync_audio(n, &config.audio);
        // AUD badge aggregate: any param has an armed audio modulation (parallels
        // has_drv / has_env). Derived after sync_audio populates audio_active.
        self.state.has_audio = self.state.mod_state.audio_active.iter().any(|&a| a);
        self.osc_addresses = config
            .params
            .iter()
            .map(|p| p.osc_address.clone())
            .collect();
        self.copied_flash.clear();
        self.slider_ids = vec![None; n];
        self.base_values = vec![0.0; n];
        self.row_catcher_ids = vec![None; n];
        self.driver_btn_ids = vec![None; n];
        self.envelope_btn_ids = vec![None; n];
        self.driver_config_ids = Vec::new();
        self.driver_config_ids.resize_with(n, || None);
        self.audio_btn_ids = vec![None; n];
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
        // Preserve the per-param tab choice across rebuilds (UI state); only grow
        // for new params. resolve_active_tab clamps stale choices at build time.
        self.mod_active_tab.resize(n, ModTab::Driver);
        self.mod_active_tab.truncate(n);
        self.mod_tab_ids = vec![Vec::new(); n];
        self.toggle_ids = Vec::new();
        self.toggle_ids.resize_with(n, || None);
        self.mapping_chevron_ids = vec![None; n];
        self.string_param_btn_ids = vec![None; config.string_params.len()];
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
    /// If `node_id` is a numeric param's value cell, build the [`PanelAction`]
    /// that opens a type-in box for it: target + id, the cell's anchor rect, the
    /// base value to prefill, the clamp range, and the int-rounding flag. Returns
    /// `None` for non-value-cell nodes and for enum params (`value_labels` use the
    /// dropdown, not text). Toggle/trigger rows carry no slider, so never match.
    pub fn value_cell_typein(&self, node_id: NodeId, tree: &UITree) -> Option<PanelAction> {
        if !self.is_live() {
            return None;
        }
        for (pi, slot) in self.slider_ids.iter().enumerate() {
            let Some(ids) = slot else { continue };
            if ids.value_text != node_id {
                continue;
            }
            let info = self.param_info.get(pi)?;
            if info.value_labels.is_some() {
                return None;
            }
            return Some(PanelAction::BeginParamTextInput {
                target: self.param_target(),
                param_id: self.pid_at(pi),
                anchor: tree.get_bounds(ids.value_text),
                value: self.base_values.get(pi).copied().unwrap_or(info.default),
                min: info.min,
                max: info.max,
                whole_numbers: info.whole_numbers,
            });
        }
        None
    }

    /// If `node_id` is a driver drawer's Free-period field, build the action that
    /// opens its beats type-in (free mode). Prefilled with the LFO's current
    /// effective period so the box opens at the live value. Returns `None`
    /// otherwise. The Free field is a type-in trigger like a value cell, so it
    /// routes here (tree-aware) rather than through `handle_click`.
    pub fn driver_period_typein(&self, node_id: NodeId, tree: &UITree) -> Option<PanelAction> {
        if !self.is_live() {
            return None;
        }
        let pi = crate::panels::param_slider_shared::driver_free_field_index(
            node_id,
            &self.driver_config_ids,
        )?;
        Some(PanelAction::BeginDriverPeriodTextInput {
            target: self.param_target(),
            param_id: self.pid_at(pi),
            anchor: tree.get_bounds(node_id),
            value: self.state.mod_state.driver_effective_period(pi),
        })
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
    /// Reset this card to "not built": the stored node range becomes empty, so
    /// [`is_live`](Self::is_live) reports false and every method that resolves or
    /// mutates by a cached node id no-ops. The inspector calls this on the cards
    /// of the inactive scope before each build — a scope it doesn't build this
    /// frame must not keep a range (or live id-handlers) aliasing the active
    /// scope's node indices.
    pub fn clear_nodes(&mut self) {
        self.first_node = usize::MAX;
        self.node_count = 0;
    }
    /// Whether this card built nodes this frame. Every cached node id (border,
    /// sliders, drag icon, drawer fields) is only valid while live; after
    /// [`clear_nodes`](Self::clear_nodes) those ids point at indices the active
    /// scope now occupies, so id-keyed methods must refuse to act on a non-live
    /// card. This is the single signal for that — keyed off the node range, the
    /// one fact the build pass keeps truthful.
    fn is_live(&self) -> bool {
        self.node_count > 0
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
    /// The header collapse-chevron node id, resolved during `build` (`None`
    /// before the first build). Exposed for headless test / automation
    /// harnesses that drive a synthetic click at the chevron and verify the
    /// collapse toggle — mirrors the already-public `mapping_chevron_rect`.
    pub fn chevron_node_id(&self) -> Option<NodeId> {
        self.chevron_btn_id
    }
    pub fn state_mut(&mut self) -> &mut ParamCardState {
        &mut self.state
    }
    pub fn set_layer_id(&mut self, id: Option<LayerId>) {
        self.layer_id = id;
    }

    /// Whether this panel already represents `config`'s effect instance. The
    /// inspector uses this to **reuse** a panel across the per-snapshot rebuild
    /// instead of allocating a fresh one — so transient UI-only state (the
    /// modulation config tab, drag, copy-flash) survives. Matches on effect
    /// identity; effect lists never carry the default id, so this is exact.
    pub(crate) fn matches_effect_config(&self, config: &ParamCardConfig) -> bool {
        self.kind == config.kind && self.effect_id == config.effect_id
    }

    /// The owning layer (generator card) — for reusing the generator panel only
    /// when the selection still points at the same layer's generator.
    pub(crate) fn owning_layer_id(&self) -> Option<&LayerId> {
        self.layer_id.as_ref()
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
            // The one inspector accent marks the selected card — same colour on
            // every card, never the per-layer lane hue.
            color::INSPECTOR_ACCENT
        } else {
            match self.kind {
                ParamCardKind::Effect => color::CARD_BORDER_C32,
                ParamCardKind::Generator => color::GEN_CARD_BORDER_C32,
            }
        }
    }

    /// Header background — the one deep-blue inspector accent for every card,
    /// regardless of kind or layer. A graph override (MOD) does NOT recolour the
    /// header; it only lights its badge.
    fn header_bg(&self) -> Color32 {
        color::INSPECTOR_HEADER_BG
    }

    /// Name-label colour for the header — white, for high contrast on the deep
    /// header blue.
    fn header_name_color(&self) -> Color32 {
        color::TEXT_WHITE_C32
    }

    /// Inner-well fill for the card's current kind + focus. The selected card
    /// lifts its well one ramp step (§19) so the edited card reads first; the
    /// rest sit at the base recessed well.
    fn base_inner_bg(&self) -> Color32 {
        let base = match self.kind {
            ParamCardKind::Effect => color::EFFECT_CARD_INNER_BG_C32,
            ParamCardKind::Generator => color::GEN_CARD_INNER_BG_C32,
        };
        if self.is_selected {
            color::lighten(base, color::FOCUS_LIFT_STEP)
        } else {
            base
        }
    }

    /// Update the border color without a full rebuild (selection highlight).
    pub fn update_selection_visual(&mut self, tree: &mut UITree, selected: bool) {
        if !self.is_live() {
            return;
        }
        if selected == self.is_selected {
            return;
        }
        self.is_selected = selected;
        if let Some(border_id) = self.border_id {
            tree.set_style(
                border_id,
                UIStyle {
                    bg_color: self.base_border_color(),
                    corner_radius: CORNER_RADIUS,
                    ..UIStyle::default()
                },
            );
        }
        // §19 focus: lift the inner well one ramp step in place, so the edited
        // card reads first without a rebuild. The well is a static panel (no
        // hover/press/text), so a bg+radius style is complete.
        if let Some(inner_id) = self.inner_bg_id {
            tree.set_style(
                inner_id,
                UIStyle {
                    bg_color: self.base_inner_bg(),
                    corner_radius: CORNER_RADIUS - BORDER_W,
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
    pub fn is_drag_handle(&self, node_id: NodeId) -> bool {
        self.is_live() && self.drag_icon_id == Some(node_id)
    }

    /// Dim/undim the card border during a reorder drag (effect kind).
    pub fn set_drag_dimmed(&self, tree: &mut UITree, dim: bool) {
        if !self.is_live() {
            return;
        }
        if let Some(border_id) = self.border_id {
            let bg_color = if dim {
                Color32::new(46, 46, 49, 100) // dimmed border
            } else {
                self.base_border_color()
            };
            tree.set_style(
                border_id,
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
        let cid = (*self.mapping_chevron_ids.get(pi)?)?;
        Some(tree.get_bounds(cid))
    }

    /// Hit-test the param NAME labels (slider + toggle/trigger rows) and return
    /// the [`ParamId`](manifold_foundation::ParamId) of the row whose label
    /// contains `(sx, sy)`, or `None`. Read-only — no behaviour change and no
    /// effect on the performance card; the graph-editor host calls it in Author
    /// context to jump from a card param straight to the node that defines it.
    pub fn label_hit(
        &self,
        tree: &UITree,
        sx: f32,
        sy: f32,
    ) -> Option<manifold_foundation::ParamId> {
        let pos = Vec2::new(sx, sy);
        for (i, info) in self.param_info.iter().enumerate() {
            let label_id = self
                .slider_ids
                .get(i)
                .and_then(|s| s.as_ref())
                .and_then(|ids| ids.label)
                .or_else(|| {
                    self.toggle_ids
                        .get(i)
                        .and_then(|t| t.as_ref())
                        .and_then(|ids| ids.label_id)
                });
            if let Some(lid) = label_id
                && tree.get_bounds(lid).contains(pos)
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
            .copied()
            .flatten()
            .map(|id| tree.get_bounds(id))
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
            h += HEADER_BODY_GAP;
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
            if !self.param_info.is_empty() || !self.string_param_info.is_empty() {
                h += HEADER_BODY_GAP;
            }
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

    /// §6b — set compact mode (hide all modulation config drawers on this card).
    /// Driven by the inspector's global "hide mod settings" toggle.
    pub fn set_compact(&mut self, compact: bool) {
        self.compact = compact;
    }

    /// Height contributed by the modulation config drawer for one slider param.
    /// Mirrors `build_param_row` exactly: 0 configs → 0; 1 → that config's
    /// height; ≥2 → the tab strip plus the single shown config (they no longer
    /// stack). Track overlays (trim bars, envelope target) add no height.
    /// Compact mode hides every drawer, so the contribution is 0.
    fn row_drawer_height(&self, i: usize) -> f32 {
        if self.compact {
            return 0.0;
        }
        let Some(info) = self.param_info.get(i) else {
            return 0.0;
        };
        let active = active_mod_tabs(&self.state.mod_state, info, i);
        match active.len() {
            0 => 0.0,
            1 => mod_config_height(active[0]),
            _ => {
                let stored = self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver);
                let shown = resolve_active_tab(&active, stored).unwrap_or(active[0]);
                MOD_TAB_STRIP_H + mod_config_height(shown)
            }
        }
    }

    // ── Build ─────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        match self.kind {
            ParamCardKind::Effect => self.build_effect(tree, rect),
            ParamCardKind::Generator => self.build_generator(tree, rect),
        }
    }

    /// The generator card as a host `View`: frame + a declarative header
    /// (`[name | Change | cog | chevron]`, right-to-left). The cog's three dots
    /// are added imperatively into the keyed cog button after build (absolute
    /// decoration that doesn't map to flow layout); in Author the cog button is a
    /// reserved transparent slot so the rest stays put.
    fn generator_card_view(&self, border_color: Color32) -> View {
        let change_style = UIStyle {
            bg_color: color::CONFIG_BG_C32,
            hover_bg_color: color::GEN_CARD_HEADER_HOVER_C32,
            pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
            text_color: color::TEXT_DIMMED_C32,
            font_size: FONT_SIZE,
            corner_radius: color::SMALL_RADIUS,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        };
        let gap = || View::panel().w(Sizing::Fixed(GAP)).fill_h();
        let cog = if self.context == CardContext::Perform {
            View::button("")
                .w(Sizing::Fixed(COG_W))
                .fill_h()
                .style(UIStyle {
                    bg_color: Color32::TRANSPARENT,
                    hover_bg_color: color::HOVER_OVERLAY,
                    pressed_bg_color: color::PRESS_OVERLAY,
                    ..UIStyle::default()
                })
                .inert()
                .key(KEY_COG)
        } else {
            View::panel().w(Sizing::Fixed(COG_W)).fill_h()
        };
        let header = View::row(0.0)
            .fill_w()
            .h(Sizing::Fixed(HEADER_HEIGHT))
            .bg(self.header_bg())
            .radius(CORNER_RADIUS - BORDER_W)
            .interactive()
            .inert()
            // §14.5 D — one right gutter: trailing controls right-align to
            // `inner_right - PADDING`, same as the effect header and the param
            // rows' value/mod-icon lane (was r: 0, flush to the inner edge).
            .pad(Pad { l: PADDING, t: 0.0, r: PADDING, b: 0.0 })
            .cross_align(Align::Center)
            .key(KEY_HEADER_BG)
            .child(
                View::label(self.name.as_str())
                    .fill_w()
                    .fill_h()
                    .font(FONT_SIZE)
                    .text_color(self.header_name_color())
                    .align_text(TextAlign::Left)
                    .interactive()
                    .inert()
                    .key(KEY_NAME),
            )
            .child(gap())
            .child(
                View::button("Change")
                    .w(Sizing::Fixed(CHANGE_BTN_W))
                    .h(Sizing::Fixed(CHANGE_BTN_H))
                    .style(change_style)
                    .inert()
                    .key(KEY_CHANGE),
            )
            .child(gap())
            .child(cog)
            .child(
                View::button(if self.is_collapsed { "\u{25B6}" } else { "\u{25BC}" })
                    .w(Sizing::Fixed(CHEVRON_W))
                    .fill_h()
                    .style(UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Center,
                        ..UIStyle::default()
                    })
                    .inert()
                    .key(KEY_CHEVRON),
            );

        View::panel()
            .fill()
            .bg(border_color)
            .radius(CORNER_RADIUS)
            .interactive()
            .inert()
            .key(KEY_BORDER)
            .pad(Pad::all(BORDER_W))
            .child(
                View::panel()
                    .fill()
                    .bg(self.base_inner_bg())
                    .radius(CORNER_RADIUS - BORDER_W)
                    .interactive()
                    .inert()
                    .key(KEY_INNER)
                    .child(header),
            )
    }

    /// Add the cog's three triangle dots as children of the keyed cog button.
    fn add_cog_dots(&self, tree: &mut UITree, cog_btn_id: NodeId) {
        let b = tree.get_bounds(cog_btn_id);
        let dot: f32 = 3.0;
        let dot_style = UIStyle {
            bg_color: color::TEXT_DIMMED_C32,
            corner_radius: dot * 0.5,
            ..UIStyle::default()
        };
        let cx = b.x + COG_W * 0.5;
        let cy = b.y + HEADER_HEIGHT * 0.5;
        let v_offset = 3.5;
        let h_offset = 4.0;
        let positions = [
            (cx - dot * 0.5, cy - v_offset - dot * 0.5),
            (cx - h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
            (cx + h_offset - dot * 0.5, cy + v_offset - dot * 0.5),
        ];
        for (px, py) in positions {
            tree.add_panel(Some(cog_btn_id), px, py, dot, dot, dot_style);
        }
    }

    /// The effect card frame on the host: border + inner bg + the header
    /// background (tinted pink when the card carries a per-card graph override).
    /// The header *contents* (drag handle, name, badges, toggle, chevron, cog)
    /// are still built imperatively into this header bg.
    fn effect_frame_view(&self, border_color: Color32) -> View {
        let header_bg = self.header_bg();
        View::panel()
            .fill()
            .bg(border_color)
            .radius(CORNER_RADIUS)
            .interactive()
            .inert()
            .key(KEY_BORDER)
            .pad(Pad::all(BORDER_W))
            .child(
                View::panel()
                    .fill()
                    .bg(self.base_inner_bg())
                    .radius(CORNER_RADIUS - BORDER_W)
                    .interactive()
                    .inert()
                    .key(KEY_INNER)
                    .child(self.effect_header_row(header_bg)),
            )
    }

    /// The effect header structure as a `View`: `[drag? | name-clip | toggle |
    /// chevron | cog?]` right-to-left. The badges, drag bars, and cog dots are
    /// added imperatively afterwards (see `build_effect_header`); the name-clip
    /// is laid `Fill` here and shrunk to leave room for active badges by the
    /// in-place re-pack, so badge behaviour is unchanged.
    fn effect_header_row(&self, header_bg: Color32) -> View {
        let author = self.context == CardContext::Author;
        let transparent_btn = |hover: Color32, pressed: Color32| UIStyle {
            bg_color: Color32::TRANSPARENT,
            hover_bg_color: hover,
            pressed_bg_color: pressed,
            ..UIStyle::default()
        };
        let mut row = View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(HEADER_HEIGHT))
            .bg(header_bg)
            .radius(CORNER_RADIUS - BORDER_W)
            .interactive()
            .inert()
            .pad(Pad { l: PADDING, t: 0.0, r: PADDING, b: 0.0 })
            .cross_align(Align::Center)
            .key(KEY_HEADER_BG);
        if !author {
            row = row.child(
                View::button("")
                    .fixed(DRAG_HANDLE_W, 16.0)
                    .style(transparent_btn(
                        color::DRAG_HANDLE_HOVER_BG_C32,
                        color::DRAG_HANDLE_BG_C32,
                    ))
                    .inert()
                    .key(KEY_DRAG),
            );
        }
        row = row
            .child(
                View::panel()
                    .clip()
                    .fill_w()
                    .h(Sizing::Fixed(16.0))
                    .key(KEY_NAME_CLIP)
                    .child(
                        View::label(self.name.as_str())
                            .fill_w()
                            .fill_h()
                            .font(FONT_SIZE)
                            .text_color(self.header_name_color())
                            .align_text(TextAlign::Left)
                            .key(KEY_NAME),
                    ),
            )
            .child(
                View::button(if self.enabled { "ON" } else { "OFF" })
                    .fixed(TOGGLE_W, 16.0)
                    .style(toggle_btn_style(self.enabled))
                    .inert()
                    .key(KEY_TOGGLE),
            );
        // Cog (or a reserved slot in Author) sits LEFT of the chevron so the
        // expand chevron is always the rightmost control — same trailing order as
        // the generator header (… · cog · ▾).
        let chevron = View::button(if self.is_collapsed { "\u{25B6}" } else { "\u{25BC}" })
            .fixed(CHEVRON_W, 16.0)
            .style(UIStyle {
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..transparent_btn(color::HOVER_OVERLAY, color::PRESS_OVERLAY)
            })
            .inert()
            .key(KEY_CHEVRON);
        if !author {
            row.child(
                View::button("")
                    .fixed(COG_W, 16.0)
                    .style(transparent_btn(color::HOVER_OVERLAY, color::PRESS_OVERLAY))
                    .inert()
                    .key(KEY_COG),
            )
            .child(chevron)
        } else {
            row.child(View::panel().w(Sizing::Fixed(COG_W)).fill_h()).child(chevron)
        }
    }


    fn build_effect(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        self.card_y = rect.y;
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.label_cache.iter_mut().for_each(|v| *v = None);

        // Card frame (border + inner bg) on the host — interactive so clicks on
        // the edge / body select the card (resolved by id in `handle_click`).
        let border_color = self.base_border_color();
        let view = self.effect_frame_view(border_color);
        let h = self.compute_height() - CARD_BOTTOM_MARGIN;
        self.host
            .build(tree, &view, Rect::new(rect.x, rect.y, rect.width, h));
        self.first_node = self.host.first_node();
        self.border_id = self.host.node_id_for_key(KEY_BORDER);
        self.inner_bg_id = self.host.node_id_for_key(KEY_INNER);
        self.header_bg_id = self.host.node_id_for_key(KEY_HEADER_BG);
        let inner = tree.get_bounds(self.inner_bg_id.expect("frame built inner bg"));
        let inner_w = inner.width;
        let parent = self.inner_bg_id.expect("frame built inner bg");

        // Header contents (badges + decorations into the host-owned header).
        self.build_effect_header(tree, inner.x, inner.y, inner_w);

        // Param sliders
        if !self.is_collapsed && !self.param_info.is_empty() {
            self.build_effect_sliders(
                tree,
                parent,
                inner.x,
                inner.y + HEADER_HEIGHT + HEADER_BODY_GAP,
                inner_w,
            );
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_effect_header(&mut self, tree: &mut UITree, x: f32, y: f32, w: f32) {
        // Header background is host-owned (see `effect_frame_view`, tinted there
        // by `has_graph_mod`); the contents below nest under it.
        let header_bg_id = self.header_bg_id.expect("header bg built by host");

        // Layout (right-to-left for fixed elements). Badges pack flush against
        // the toggle — only the active ones take a slot — so the name cell is
        // as wide as possible and a lone badge never floats mid-header.
        // Trailing order (right→left): chevron (always rightmost), cog, toggle —
        // matches the host View child order in `effect_header_row`.
        let chevron_x = x + w - PADDING - CHEVRON_W;
        let cog_x = chevron_x - GAP - COG_W;
        let toggle_x = cog_x - GAP - TOGGLE_W;
        // Left edge of the name/badge region — after the drag handle (perform) or
        // at the padding (author, no drag handle).
        let content_left = x + PADDING
            + if self.context == CardContext::Author { 0.0 } else { DRAG_HANDLE_W + GAP };
        let badges = effect_badge_layout(
            content_left,
            toggle_x,
            self.state.has_graph_mod,
            self.state.has_abl,
            self.state.has_env,
            self.state.has_drv,
            self.state.has_audio,
        );
        let badge_park = toggle_x - GAP - BADGE_W;
        let mod_x = badges.mod_x.unwrap_or(badge_park);
        let abl_x = badges.abl_x.unwrap_or(badge_park);
        let env_x = badges.env_x.unwrap_or(badge_park);
        let drv_x = badges.drv_x.unwrap_or(badge_park);
        let aud_x = badges.aud_x.unwrap_or(badge_park);
        let elem_y = y + (HEADER_HEIGHT - 16.0) * 0.5;
        let badge_y = y + (HEADER_HEIGHT - BADGE_H) * 0.5;

        // The header structure (drag handle, name-clip + label, toggle, chevron,
        // cog) is host-built (see `effect_header_row`); resolve its ids by key.
        // The badges, the drag bars, and the cog dots below are the imperative
        // decorations layered on top.
        self.drag_icon_id = self.host.node_id_for_key(KEY_DRAG);
        self.name_clip_id = self.host.node_id_for_key(KEY_NAME_CLIP);
        self.name_label_id = self.host.node_id_for_key(KEY_NAME);
        self.toggle_btn_id = self.host.node_id_for_key(KEY_TOGGLE);
        self.chevron_btn_id = self.host.node_id_for_key(KEY_CHEVRON);
        self.cog_btn_id = self.host.node_id_for_key(KEY_COG);

        // Drag-handle bars (3 horizontal lines) into the host drag button.
        if let Some(drag_icon_id) = self.drag_icon_id {
            let dh_x = x + PADDING;
            let bar_w: f32 = 10.0;
            let bar_h: f32 = 1.5;
            let bar_x = dh_x + (DRAG_HANDLE_W - bar_w) * 0.5;
            let bar_style = UIStyle {
                bg_color: color::TEXT_DIMMED_C32,
                ..UIStyle::default()
            };
            for i in 0..3 {
                let bar_y = elem_y + 3.5 + i as f32 * 3.5;
                tree.add_panel(Some(drag_icon_id), bar_x, bar_y, bar_w, bar_h, bar_style);
            }
        }

        // ABL badge — visibility synced from state.has_abl
        let show_abl = self.state.has_abl;
        let abl_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            abl_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::ABL_BADGE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.abl_badge_bg_id = Some(abl_badge_bg_id);
        let abl_badge_text_id = tree.add_label(
            Some(abl_badge_bg_id),
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
        );
        self.abl_badge_text_id = Some(abl_badge_text_id);
        tree.set_visible(abl_badge_bg_id, show_abl);
        tree.set_visible(abl_badge_text_id, show_abl);

        // ENV badge — visibility synced from state.has_env
        let show_env = self.state.has_env;
        let env_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            env_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::ENVELOPE_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.env_badge_bg_id = Some(env_badge_bg_id);
        let env_badge_text_id = tree.add_label(
            Some(env_badge_bg_id),
            env_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "TRG",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.env_badge_text_id = Some(env_badge_text_id);
        tree.set_visible(env_badge_bg_id, show_env);
        tree.set_visible(env_badge_text_id, show_env);

        // DRV badge — visibility synced from state.has_drv
        let show_drv = self.state.has_drv;
        let drv_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            drv_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::DRIVER_ACTIVE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.drv_badge_bg_id = Some(drv_badge_bg_id);
        let drv_badge_text_id = tree.add_label(
            Some(drv_badge_bg_id),
            drv_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "LFO",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.drv_badge_text_id = Some(drv_badge_text_id);
        tree.set_visible(drv_badge_bg_id, show_drv);
        tree.set_visible(drv_badge_text_id, show_drv);

        // MOD badge — pink chip indicating the card's graph topology
        // diverges from the catalog default.
        let show_mod = self.state.has_graph_mod;
        let mod_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            mod_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::MOD_BADGE_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.mod_badge_bg_id = Some(mod_badge_bg_id);
        let mod_badge_text_id = tree.add_label(
            Some(mod_badge_bg_id),
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
        );
        self.mod_badge_text_id = Some(mod_badge_text_id);
        tree.set_visible(mod_badge_bg_id, show_mod);
        tree.set_visible(mod_badge_text_id, show_mod);

        // AUD badge — green chip, matching the audio "A" arm button; shows when
        // any param on the card has an armed audio modulation.
        let show_aud = self.state.has_audio;
        let aud_badge_bg_id = tree.add_panel(
            Some(header_bg_id),
            aud_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            UIStyle {
                bg_color: color::AUDIO_TRIM_BAR_C32,
                corner_radius: BADGE_RADIUS,
                ..UIStyle::default()
            },
        );
        self.aud_badge_bg_id = Some(aud_badge_bg_id);
        let aud_badge_text_id = tree.add_label(
            Some(aud_badge_bg_id),
            aud_x,
            badge_y,
            BADGE_W,
            BADGE_H,
            "AUD",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: color::FONT_CAPTION,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        self.aud_badge_text_id = Some(aud_badge_text_id);
        tree.set_visible(aud_badge_bg_id, show_aud);
        tree.set_visible(aud_badge_text_id, show_aud);

        self.cached_has_env = show_env;
        self.cached_has_drv = show_drv;
        self.cached_has_abl = show_abl;
        self.cached_has_audio = show_aud;
        self.cached_has_graph_mod = show_mod;
        self.cached_enabled = self.enabled;

        // Cog dots (three in a triangle) into the host cog button.
        if let Some(cog_btn_id) = self.cog_btn_id {
            let dot: f32 = 3.0;
            let dot_style = UIStyle {
                bg_color: color::TEXT_DIMMED_C32,
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
                tree.add_panel(Some(cog_btn_id), px, py, dot, dot, dot_style);
            }
        }

        // Shrink the host name-clip to leave room for the active badges and
        // settle the badge positions — the same in-place re-pack `sync` runs, so
        // badge behaviour is unchanged.
        self.reposition_effect_badges(tree);
    }

    fn build_effect_sliders(
        &mut self,
        tree: &mut UITree,
        parent: NodeId,
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
        // Lane = slider→group gap + 3 buttons + 2 inter-button gaps.
        let slider_w = w
            - PADDING * 2.0
            - MOD_LANE_GAP
            - DE_BUTTON_SIZE * 3.0
            - DE_BUTTON_GAP * 2.0
            - chevron_lane;

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
                Some(parent),
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
                self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
                !self.compact,
                author.then_some((i as u64) << 8),
            );
            self.slider_ids[i] = row.slider;
            self.row_catcher_ids[i] = Some(row.row_catcher);
            self.trim_ids[i] = row.trim;
            self.target_ids[i] = row.target;
            self.envelope_config_ids[i] = row.envelope_config;
            self.ableton_trim_ids[i] = row.ableton_trim;
            self.audio_trim_ids[i] = row.audio_trim;
            self.envelope_btn_ids[i] = row.envelope_btn;
            self.driver_btn_ids[i] = Some(row.driver_btn);
            self.driver_config_ids[i] = row.driver_config;
            self.ableton_config_ids[i] = row.ableton_config;
            self.audio_btn_ids[i] = Some(row.audio_btn);
            self.audio_configs[i] = row.audio_config;
            self.mod_tab_ids[i] = row.mod_tabs;
            // Mapping-drawer chevron at the row's right edge (Author + mappable).
            // A subtle ">" that opens the sideways range/scale/offset/invert/
            // curve drawer for this binding. Sits past the D/E buttons in the
            // reserved lane; click resolves via `mapping_chevron_ids`.
            if author && info.mappable {
                let ch_x = x + PADDING + (w - PADDING * 2.0) - MAP_CHEVRON_W;
                let ch_y = row_y + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
                // Keyed by (row | chevron role): the chevron's identity must not
                // shift when an earlier row arms a modulator and inserts drawer
                // nodes ahead of it. See `docs/INPUT_IDENTITY_UNIFICATION.md`.
                self.mapping_chevron_ids[i] = Some(tree.add_button_keyed(
                    Some(parent),
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
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                    "\u{203A}", // ›
                    ((i as u64) << 8) | ROW_ROLE_CHEVRON,
                ));
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

        // ── Card frame + header (host) ──
        let border_color = self.base_border_color();
        let view = self.generator_card_view(border_color);
        let h = self.compute_height() - CARD_BOTTOM_MARGIN;
        self.host
            .build(tree, &view, Rect::new(rect.x, rect.y, rect.width, h));
        self.first_node = self.host.first_node();
        self.border_id = self.host.node_id_for_key(KEY_BORDER);
        self.inner_bg_id = self.host.node_id_for_key(KEY_INNER);
        self.header_bg_id = self.host.node_id_for_key(KEY_HEADER_BG);
        self.name_label_id = self.host.node_id_for_key(KEY_NAME);
        self.change_btn_id = self.host.node_id_for_key(KEY_CHANGE);
        self.chevron_btn_id = self.host.node_id_for_key(KEY_CHEVRON);
        self.cog_btn_id = self.host.node_id_for_key(KEY_COG);
        if let Some(cog) = self.cog_btn_id {
            self.add_cog_dots(tree, cog);
        }

        let inner_x = rect.x + BORDER_W;
        let inner_y = rect.y + BORDER_W;
        let inner_w = rect.width - BORDER_W * 2.0;

        // ── Params (if not collapsed) ──
        if !self.is_collapsed && !self.param_info.is_empty() {
            let content_w = inner_w - PADDING * 2.0;
            let cx = inner_x + PADDING;
            let mut cy = inner_y + HEADER_HEIGHT + HEADER_BODY_GAP;
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
            let slider_w = content_w
                - MOD_LANE_GAP
                - DE_BUTTON_SIZE * 3.0
                - DE_BUTTON_GAP * 2.0
                - chevron_lane;
            // Same growth rule as the effect card (see build_effect_sliders).
            let label_width = crate::slider::label_width_for_row(content_w);

            for i in 0..self.param_info.len() {
                let info = self.param_info[i].clone();

                if info.is_toggle || info.is_trigger {
                    // Toggle / Trigger row — both share the button-row layout.
                    // ON/OFF for sticky toggles, ▶ for momentary fire-once
                    // triggers. Click handler dispatches differently (toggle vs
                    // fire) based on the is_trigger flag.
                    //
                    // §6.4: line the toggle up with the slider grid — its button
                    // right-aligns to the same control column as slider VALUES
                    // (x = cx + slider_w), so it doesn't float at the far edge and
                    // read as bolted-on. A toggle can't be modulated, so the
                    // D/E/A lane to its right is correctly left empty. The label
                    // fills the column to the button's left (left-aligned, same
                    // start x as every slider label).
                    let toggle_btn_x = cx + slider_w - TOGGLE_BTN_W;
                    let label_id = tree.add_label(
                        None,
                        cx,
                        cy,
                        (slider_w - TOGGLE_BTN_W - GAP).max(0.0),
                        ROW_HEIGHT,
                        &info.name,
                        UIStyle {
                            text_color: color::SLIDER_TEXT_C32,
                            font_size: FONT_SIZE,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
                    );

                    let on = info.default > 0.5;
                    let (button_text, button_style) = if info.is_trigger {
                        // Trigger renders as a momentary button — always neutral.
                        ("▶", toggle_btn_style(false))
                    } else {
                        (if on { "ON" } else { "OFF" }, toggle_btn_style(on))
                    };
                    let toggle_y = cy + (ROW_HEIGHT - TOGGLE_BTN_H) * 0.5;
                    let button_id = match author.then_some(((i as u64) << 8) | ROW_ROLE_TOGGLE) {
                        Some(key) => tree.add_button_keyed(
                            None,
                            toggle_btn_x,
                            toggle_y,
                            TOGGLE_BTN_W,
                            TOGGLE_BTN_H,
                            button_style,
                            button_text,
                            key,
                        ),
                        None => tree.add_button(
                            None,
                            toggle_btn_x,
                            toggle_y,
                            TOGGLE_BTN_W,
                            TOGGLE_BTN_H,
                            button_style,
                            button_text,
                        ),
                    };

                    // Make toggle label interactive for click-to-copy OSC address
                    if self.osc_addresses.get(i).and_then(|a| a.as_ref()).is_some() {
                        tree.set_flag(label_id, UIFlags::INTERACTIVE);
                    }

                    self.toggle_ids[i] = Some(ToggleParamIds {
                        label_id: Some(label_id),
                        button_id,
                    });
                    self.toggle_cache[i] = on;
                    cy += ROW_HEIGHT + ROW_SPACING;
                } else {
                    // Slider row — shared per-param core. Generators parent rows
                    // flat to the root (`None`), use the gen-param slider palette,
                    // the body-size driver-config font, and always show the `E`
                    // button (generators always support envelopes).
                    let row_y = cy;
                    let row = build_param_row(
                        tree,
                        None,
                        cx,
                        cy,
                        slider_w,
                        content_w,
                        &info,
                        &self.state.mod_state,
                        i,
                        &SliderColors::default_slider(),
                        FONT_SIZE,
                        true,
                        label_width,
                        self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
                        !self.compact,
                        author.then_some((i as u64) << 8),
                    );
                    self.slider_ids[i] = row.slider;
                    self.row_catcher_ids[i] = Some(row.row_catcher);
                    self.trim_ids[i] = row.trim;
                    self.target_ids[i] = row.target;
                    self.envelope_config_ids[i] = row.envelope_config;
                    self.ableton_trim_ids[i] = row.ableton_trim;
                    self.audio_trim_ids[i] = row.audio_trim;
                    self.envelope_btn_ids[i] = row.envelope_btn;
                    self.driver_btn_ids[i] = Some(row.driver_btn);
                    self.driver_config_ids[i] = row.driver_config;
                    self.ableton_config_ids[i] = row.ableton_config;
                    self.audio_btn_ids[i] = Some(row.audio_btn);
                    self.audio_configs[i] = row.audio_config;
                    self.mod_tab_ids[i] = row.mod_tabs;
                    // Mapping-drawer chevron at the row's right edge (Author +
                    // mappable) — identical to the effect card. Opens the same
                    // sideways range/scale/offset/invert/curve drawer; click
                    // resolves via the shared `mapping_chevron_ids`.
                    if author && info.mappable {
                        let ch_x = cx + content_w - MAP_CHEVRON_W;
                        let ch_y = row_y + (ROW_HEIGHT - DE_BUTTON_SIZE) * 0.5;
                        self.mapping_chevron_ids[i] = Some(tree.add_button_keyed(
                            None,
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
                                corner_radius: color::SMALL_RADIUS,
                                ..UIStyle::default()
                            },
                            "\u{203A}", // ›
                            ((i as u64) << 8) | ROW_ROLE_CHEVRON,
                        ));
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
                self.string_param_btn_ids[si] = Some(tree.add_button(
                    None,
                    cx,
                    cy,
                    content_w,
                    ROW_HEIGHT,
                    UIStyle {
                        bg_color: color::INSPECTOR_BG,
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                    &display,
                ));
                cy += ROW_HEIGHT + ROW_SPACING;
            }
        } // end if !self.is_collapsed

        self.node_count = tree.count() - self.first_node;
    }

    // ── Sync methods ──────────────────────────────────────────────

    pub fn sync_values(&mut self, tree: &mut UITree, values: &[crate::view::UiParamSlot]) {
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
        let (Some(toggle_btn_id), Some(name_clip_id), Some(mod_badge_bg_id)) =
            (self.toggle_btn_id, self.name_clip_id, self.mod_badge_bg_id)
        else {
            return;
        };
        let toggle_x = tree.get_bounds(toggle_btn_id).x;
        let badge_y = tree.get_bounds(mod_badge_bg_id).y;
        // The name cell's left edge is the region's left bound for centering.
        let content_left = tree.get_bounds(name_clip_id).x;
        let badges = effect_badge_layout(
            content_left,
            toggle_x,
            self.state.has_graph_mod,
            self.state.has_abl,
            self.state.has_env,
            self.state.has_drv,
            self.state.has_audio,
        );
        let park = toggle_x - GAP - BADGE_W;
        for (bg, txt, x) in [
            (self.mod_badge_bg_id, self.mod_badge_text_id, badges.mod_x),
            (self.abl_badge_bg_id, self.abl_badge_text_id, badges.abl_x),
            (self.env_badge_bg_id, self.env_badge_text_id, badges.env_x),
            (self.drv_badge_bg_id, self.drv_badge_text_id, badges.drv_x),
            (self.aud_badge_bg_id, self.aud_badge_text_id, badges.aud_x),
        ] {
            let r = Rect::new(x.unwrap_or(park), badge_y, BADGE_W, BADGE_H);
            if let Some(bg) = bg {
                tree.set_bounds(bg, r);
            }
            if let Some(txt) = txt {
                tree.set_bounds(txt, r);
            }
        }
        let name_b = tree.get_bounds(name_clip_id);
        let name_w = (badges.name_right - name_b.x).max(10.0);
        tree.set_bounds(
            name_clip_id,
            Rect::new(name_b.x, name_b.y, name_w, name_b.height),
        );
    }

    fn sync_values_effect(
        &mut self,
        tree: &mut UITree,
        values: &[crate::view::UiParamSlot],
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
                            .filter(|ids| ids.label == Some(label_id))
                            .and_then(|_| self.param_info.get(pi).map(|p| p.name.clone()))
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        // Toggle state dirty-check
        if self.enabled != self.cached_enabled {
            self.cached_enabled = self.enabled;
            if let Some(toggle_btn_id) = self.toggle_btn_id {
                tree.set_style(toggle_btn_id, toggle_btn_style(self.enabled));
                tree.set_text(toggle_btn_id, if self.enabled { "ON" } else { "OFF" });
            }
        }

        // Badge visibility dirty-check
        if self.state.has_env != self.cached_has_env
            || self.state.has_drv != self.cached_has_drv
            || self.state.has_abl != self.cached_has_abl
            || self.state.has_audio != self.cached_has_audio
            || self.state.has_graph_mod != self.cached_has_graph_mod
        {
            self.cached_has_env = self.state.has_env;
            self.cached_has_drv = self.state.has_drv;
            self.cached_has_abl = self.state.has_abl;
            self.cached_has_audio = self.state.has_audio;
            self.cached_has_graph_mod = self.state.has_graph_mod;
            for (id, visible) in [
                (self.abl_badge_bg_id, self.cached_has_abl),
                (self.abl_badge_text_id, self.cached_has_abl),
                (self.env_badge_bg_id, self.cached_has_env),
                (self.env_badge_text_id, self.cached_has_env),
                (self.drv_badge_bg_id, self.cached_has_drv),
                (self.drv_badge_text_id, self.cached_has_drv),
                (self.aud_badge_bg_id, self.cached_has_audio),
                (self.aud_badge_text_id, self.cached_has_audio),
                (self.mod_badge_bg_id, self.cached_has_graph_mod),
                (self.mod_badge_text_id, self.cached_has_graph_mod),
            ] {
                if let Some(id) = id {
                    tree.set_visible(id, visible);
                }
            }
            // Re-pack the badges + resize the name cell now the active set changed.
            // The header keeps the one accent — a graph override lights the MOD
            // badge only, it never recolours the header.
            self.reposition_effect_badges(tree);
        }

        // Skip slider sync if collapsed
        if self.is_collapsed {
            return;
        }

        // Per-param slider values + label (dirty-check via param_cache / label_cache)
        for (i, slot) in values.iter().enumerate().take(self.param_info.len()) {
            let val = slot.value;
            if let Some(b) = self.base_values.get_mut(i) {
                *b = slot.base;
            }
            let info = &self.param_info[i];
            let new_label = Some(info.name.clone());

            // Label dirty-check
            if self.label_cache[i] != new_label {
                self.label_cache[i] = new_label;
                if let Some(ref ids) = self.slider_ids[i]
                    && let Some(label) = ids.label
                {
                    tree.set_text(label, &info.name);
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
        values: &[crate::view::UiParamSlot],
    ) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| self.find_label_name(label_id))
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        for (i, slot) in values.iter().enumerate().take(self.param_info.len()) {
            let val = slot.value;
            if let Some(b) = self.base_values.get_mut(i) {
                *b = slot.base;
            }
            let info = &self.param_info[i];

            // Label dirty-check (slider rows only — toggle/trigger rows have
            // their label baked into the row at build time).
            if !info.is_toggle && !info.is_trigger {
                let new_label = Some(info.name.clone());
                if self.label_cache[i] != new_label {
                    self.label_cache[i] = new_label;
                    if let Some(ref ids) = self.slider_ids[i]
                        && let Some(label) = ids.label
                    {
                        tree.set_text(label, &info.name);
                    }
                }
            }

            if info.is_toggle {
                let on = val > 0.5;
                if on != self.toggle_cache[i] {
                    self.toggle_cache[i] = on;
                    if let Some(ref ids) = self.toggle_ids[i] {
                        tree.set_style(ids.button_id, toggle_btn_style(on));
                        tree.set_text(ids.button_id, if on { "ON" } else { "OFF" });
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
    fn find_label_name(&self, label_id: NodeId) -> String {
        for (pi, s) in self.slider_ids.iter().enumerate() {
            if let Some(ids) = s
                && ids.label == Some(label_id)
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
                && ids.label_id == Some(label_id)
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
        if let Some(name_label_id) = self.name_label_id {
            tree.set_text(name_label_id, name);
        }
    }

    pub fn sync_enabled(&mut self, _tree: &mut UITree, enabled: bool) {
        // Just update the field — tree update happens in sync_values() dirty-check.
        self.enabled = enabled;
    }

    pub fn sync_gen_type_name(&mut self, tree: &mut UITree, name: &str) {
        self.name = name.into();
        if let Some(name_label_id) = self.name_label_id {
            tree.set_text(name_label_id, name);
        }
    }

    /// Update a string param value and its display text (generator kind).
    pub fn sync_string_param(&mut self, tree: &mut UITree, index: usize, value: &str) {
        if let Some(sp) = self.string_param_info.get_mut(index) {
            sp.value = value.to_string();
            if let Some(Some(btn_id)) = self.string_param_btn_ids.get(index).copied() {
                let display = if value.is_empty() {
                    format!("{}: (empty)", sp.name)
                } else {
                    format!("{}: {}", sp.name, value)
                };
                tree.set_text(btn_id, &display);
            }
        }
    }

    // ── Event handling ────────────────────────────────────────────

    /// Resolve the panel-local positional `pi` back to its stable
    /// [`ParamId`](manifold_foundation::ParamId) for outbound
    /// [`PanelAction`] emission. The panel's per-widget bookkeeping is
    /// legitimately positional (it indexes `param_info`, `driver_btn_ids`,
    /// etc.); this is the one helper that keeps that off the wire format.
    #[inline]
    fn pid_at(&self, pi: usize) -> manifold_foundation::ParamId {
        self.param_info[pi].param_id.clone()
    }

    /// Match a clicked node against this card's modulation-config tab strips
    /// (only present on rows with ≥2 active configs). Returns the param index and
    /// the tab the click selects.
    fn mod_tab_hit(&self, id: NodeId) -> Option<(usize, ModTab)> {
        self.mod_tab_ids.iter().enumerate().find_map(|(pi, tabs)| {
            tabs.iter()
                .find(|(tid, _)| *tid == id)
                .map(|&(_, t)| (pi, t))
        })
    }

    /// Point param `pi`'s config drawer at `tab` — used when arming a modulator
    /// so its config comes forward. No-op if `pi` is out of range.
    fn focus_mod_tab(&mut self, pi: usize, tab: ModTab) {
        if let Some(slot) = self.mod_active_tab.get_mut(pi) {
            *slot = tab;
        }
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
        let feature = crate::types::AudioFeature::new(
            audio_kind_from_index(kind_idx),
            audio_band_from_index(band_idx),
        );
        vec![PanelAction::AudioModSetSource(target, self.pid_at(pi), send_id, feature)]
    }

    pub fn handle_click(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        match self.kind {
            ParamCardKind::Effect => self.handle_click_effect(node_id),
            ParamCardKind::Generator => self.handle_click_generator(node_id),
        }
    }

    fn handle_click_effect(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        let id = node_id;
        let ei = self.effect_index;

        // Header buttons
        if self.toggle_btn_id == Some(id) {
            return vec![PanelAction::EffectToggle(ei)];
        }
        if self.chevron_btn_id == Some(id) {
            return vec![PanelAction::EffectCollapseToggle(ei)];
        }
        if self.cog_btn_id == Some(id) {
            return vec![PanelAction::OpenGraphEditor(ei)];
        }

        // Mapping-drawer chevron (Author context) → open the sideways
        // range/scale/offset/invert/curve drawer for this row's binding.
        if let Some(pi) = self
            .mapping_chevron_ids
            .iter()
            .position(|&cid| cid == Some(id))
        {
            return vec![PanelAction::OpenCardMapping(self.pid_at(pi))];
        }

        // Modulation config tab strip (≥2 configs active) → switch which config
        // the shared drawer shows. UI-only state; a rebuild repaints it.
        if let Some((pi, tab)) = self.mod_tab_hit(id) {
            if let Some(slot) = self.mod_active_tab.get_mut(pi) {
                *slot = tab;
            }
            return vec![PanelAction::ModConfigTabChanged];
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
                    self.focus_mod_tab(pi, ModTab::Driver);
                    vec![PanelAction::DriverToggle(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::EnvelopeToggle(pi) => {
                    self.focus_mod_tab(pi, ModTab::Envelope);
                    vec![PanelAction::EnvelopeToggle(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::DriverConfig(pi, action) => {
                    vec![PanelAction::DriverConfig(GraphParamTarget::Effect(ei), self.pid_at(pi), action)]
                }
                RowClick::AbletonInvert(pi) => {
                    vec![PanelAction::AbletonInvertToggle(GraphParamTarget::Effect(ei), self.pid_at(pi))]
                }
                RowClick::AudioToggle(pi) => {
                    self.focus_mod_tab(pi, ModTab::Audio);
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
                    if let Some(ids) = &self.slider_ids[pi]
                        && let Some(label) = ids.label
                    {
                        self.copied_flash.trigger(label);
                    }
                    let addr = self.osc_addresses[pi].clone().unwrap_or_default();
                    vec![PanelAction::CopyOscAddress(addr)]
                }
            };
        }

        // Card selection — any click on card background, border, or header
        if self.border_id == Some(id)
            || self.header_bg_id == Some(id)
            || self.inner_bg_id == Some(id)
            || self.drag_icon_id == Some(id)
            || self.name_label_id == Some(id)
        {
            return vec![PanelAction::EffectCardClicked(ei)];
        }

        Vec::new()
    }

    fn handle_click_generator(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        let id = node_id;

        // Chevron → collapse/expand
        if self.chevron_btn_id == Some(id) {
            return vec![PanelAction::GenCollapseToggle];
        }

        // Change button → open type picker
        if self.change_btn_id == Some(id) {
            return vec![PanelAction::GenTypeClicked(self.layer_id.clone())];
        }

        // Cog → open graph editor for this generator
        if self.cog_btn_id == Some(id) {
            return vec![PanelAction::OpenGeneratorGraphEditor];
        }

        // Mapping-drawer chevron (Author context) → open the sideways
        // range/scale/offset/invert/curve drawer for this row's param. Same
        // action the effect card emits; the host resolves it against the
        // watched generator target (the unified mapping surface).
        if let Some(pi) = self
            .mapping_chevron_ids
            .iter()
            .position(|&cid| cid == Some(id))
        {
            return vec![PanelAction::OpenCardMapping(self.pid_at(pi))];
        }

        // Card click (header bg, name, border) → select the card
        if self.header_bg_id == Some(id)
            || self.name_label_id == Some(id)
            || self.border_id == Some(id)
        {
            return vec![PanelAction::GenCardClicked];
        }

        // Toggle / Trigger buttons — same button slot, different semantics.
        // is_trigger fires GenParamFire (counter +1); is_toggle fires
        // GenParamToggle (0↔1 flip).
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && t.button_id == id
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

        // Modulation config tab strip (≥2 configs active) → switch shown config.
        if let Some((pi, tab)) = self.mod_tab_hit(id) {
            self.focus_mod_tab(pi, tab);
            return vec![PanelAction::ModConfigTabChanged];
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
                RowClick::DriverToggle(pi) => {
                    self.focus_mod_tab(pi, ModTab::Driver);
                    vec![PanelAction::DriverToggle(GraphParamTarget::Generator, self.pid_at(pi))]
                }
                RowClick::EnvelopeToggle(pi) => {
                    self.focus_mod_tab(pi, ModTab::Envelope);
                    vec![PanelAction::EnvelopeToggle(GraphParamTarget::Generator, self.pid_at(pi))]
                }
                RowClick::DriverConfig(pi, action) => {
                    vec![PanelAction::DriverConfig(GraphParamTarget::Generator, self.pid_at(pi), action)]
                }
                RowClick::AbletonInvert(pi) => {
                    vec![PanelAction::AbletonInvertToggle(GraphParamTarget::Generator, self.pid_at(pi))]
                }
                RowClick::AudioToggle(pi) => {
                    self.focus_mod_tab(pi, ModTab::Audio);
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
                    if let Some(ids) = &self.slider_ids[pi]
                        && let Some(label) = ids.label
                    {
                        self.copied_flash.trigger(label);
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
                && t.label_id == Some(id)
                && let Some(addr) = self.osc_addresses.get(pi).and_then(|a| a.clone())
            {
                self.copied_flash.trigger(id);
                return vec![PanelAction::CopyOscAddress(addr)];
            }
        }

        // String param buttons → open text input or dropdown
        for (si, &btn_id) in self.string_param_btn_ids.iter().enumerate() {
            if btn_id == Some(id) {
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
    pub fn handle_pointer_down(&mut self, node_id: NodeId, pos: Vec2) -> Vec<PanelAction> {
        let target = self.param_target();

        // 1. Envelope target handle (the orange grab bar on the slider track).
        for (pi, etarget) in self.target_ids.iter().enumerate() {
            if let Some(t) = etarget
                && node_id == t.target_bar_id
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
                    if node_id == t.min_bar_id {
                        trim_hit = Some((kind, pi, true));
                        break 'trim;
                    }
                    if node_id == t.max_bar_id {
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
                            node_id == t.fill_id
                                || node_id == t.min_bar_id
                                || node_id == t.max_bar_id
                        })
                        || self
                            .target_ids
                            .get(pi)
                            .and_then(|t| t.as_ref())
                            .is_some_and(|t| node_id == t.target_bar_id)
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
                        t.target_bar_id,
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
        if !self.is_live() {
            return;
        }
        use crate::intent::Gesture::RightClick;

        let target = match self.kind {
            ParamCardKind::Effect => GraphParamTarget::Effect(self.effect_index),
            ParamCardKind::Generator => GraphParamTarget::Generator,
        };

        // Card root: claim the whole area + the context-menu action. Any
        // descendant without a more specific intent folds here.
        if let Some(border_id) = self.border_id {
            intents.claim_area(border_id);
            intents.on(border_id, RightClick, PanelAction::CardRightClicked(target));
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
                if let Some(label) = ids.label {
                    intents.on(label, RightClick, menu.clone());
                }
                if let Some(Some(catcher)) = self.row_catcher_ids.get(pi).copied() {
                    intents.claim_area(catcher);
                    intents.on(catcher, RightClick, menu);
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
            driver_free_period: vec![None; n],
            audio: Default::default(),
        }
    }

    #[test]
    fn build_effect_card() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        assert!(panel.border_id.is_some());
        assert!(panel.inner_bg_id.is_some());
        assert!(panel.header_bg_id.is_some());
        assert!(panel.drag_icon_id.is_some());
        assert!(panel.name_label_id.is_some());
        assert!(panel.toggle_btn_id.is_some());
        assert!(panel.chevron_btn_id.is_some());
        assert_eq!(panel.slider_ids.len(), 2);
        assert!(panel.slider_ids[0].is_some());
        assert!(panel.slider_ids[1].is_some());
        assert!(panel.node_count > 0);
    }

    #[test]
    fn focused_effect_card_lifts_inner_well() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));
        // Unfocused: the well sits at the base recessed colour.
        assert_eq!(panel.base_inner_bg(), color::EFFECT_CARD_INNER_BG_C32);
        // Focus it → the well lifts one ramp step (the same lift the timeline
        // lane gets), so the edited card reads first.
        panel.update_selection_visual(&mut tree, true);
        assert_eq!(
            panel.base_inner_bg(),
            color::lighten(color::EFFECT_CARD_INNER_BG_C32, color::FOCUS_LIFT_STEP)
        );
        assert_eq!(panel.base_border_color(), color::SELECTED_BORDER);
    }

    /// After `clear_nodes` (the inspector resetting an inactive scope's card),
    /// every id-keyed method must no-op — even with its real cached ids — so a
    /// card that didn't build this frame can't resolve or mutate by indices the
    /// active scope now owns. This is the self-inert half of range truthfulness.
    #[test]
    fn cleared_card_is_inert_to_id_methods() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let value_cell = panel.slider_ids[0].as_ref().unwrap().value_text;
        let drag_icon = panel.drag_icon_id.unwrap();
        let border = panel.border_id.unwrap();

        // While live: the value cell opens a type-in and the drag icon is known.
        assert!(panel.value_cell_typein(value_cell, &tree).is_some());
        assert!(panel.is_drag_handle(drag_icon));

        panel.clear_nodes();
        assert_eq!(panel.node_count(), 0);

        // Not live → every id-keyed method no-ops, even though the cached ids
        // still point at real (live-in-tree) nodes.
        assert!(panel.value_cell_typein(value_cell, &tree).is_none());
        assert!(panel.driver_period_typein(value_cell, &tree).is_none());
        assert!(!panel.is_drag_handle(drag_icon));

        // update_selection_visual must not repaint when the card isn't live.
        let before = tree.get_node(border).style.bg_color;
        panel.update_selection_visual(&mut tree, true);
        assert_eq!(
            tree.get_node(border).style.bg_color,
            before,
            "a non-live card must not repaint its border"
        );
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
        assert!(panel.cog_btn_id.is_none(), "cog suppressed in Author");
        assert!(
            panel.drag_icon_id.is_none(),
            "drag handle suppressed in Author"
        );
        // Mapping chevron only on the mappable row.
        assert!(panel.mapping_chevron_ids[0].is_none(), "row 0 not mappable");
        assert!(
            panel.mapping_chevron_ids[1].is_some(),
            "row 1 mappable → chevron"
        );
    }

    #[test]
    fn perform_context_has_no_mapping_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new(); // default Perform
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        // Perform keeps the cog and never draws the mapping chevron.
        assert!(panel.cog_btn_id.is_some(), "cog present in Perform");
        assert!(panel.mapping_chevron_ids.iter().all(|id| id.is_none()));
    }

    #[test]
    fn mapping_chevron_click_emits_open_card_mapping() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.set_context(CardContext::Author);
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        let chevron = panel.mapping_chevron_ids[1].expect("row 1 mappable → chevron");
        let actions = panel.handle_click(chevron);
        assert!(
            matches!(&actions[..], [PanelAction::OpenCardMapping(pid)] if pid == "strength"),
            "got {actions:?}"
        );
        // The chevron also has a resolvable anchor rect by binding id.
        assert!(panel.mapping_chevron_rect(&tree, "strength").is_some());
        assert!(panel.mapping_chevron_rect(&tree, "radius").is_none());
    }

    /// Phase 3: the editor chevron is keyed by (row | role), so its durable
    /// WidgetId must not move when an EARLIER row arms a modulator — which inserts
    /// drawer + track-overlay nodes ahead of it and shifts every later sibling.
    /// An auto-salted node would renumber; the keyed chevron does not.
    #[test]
    fn editor_chevron_identity_survives_earlier_row_arming_a_mod() {
        let build_with_driver0 = |driver0: bool| {
            let mut tree = UITree::new();
            let mut c = effect_config_with_mappable(); // param[1] is mappable
            c.driver_active = vec![driver0, false];
            let mut panel = ParamCardPanel::new();
            panel.set_context(CardContext::Author);
            panel.configure(&c);
            panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 300.0));
            let chevron = panel.mapping_chevron_ids[1].expect("row 1 mappable → chevron");
            (tree.widget_of(chevron), tree.count())
        };

        let (w_plain, n_plain) = build_with_driver0(false);
        let (w_armed, n_armed) = build_with_driver0(true);

        assert!(
            n_armed > n_plain,
            "arming row 0's driver must add drawer/overlay nodes (a real sibling shift)"
        );
        assert_eq!(
            w_plain, w_armed,
            "keyed chevron identity must be stable despite the earlier row's structural change"
        );
        assert_ne!(w_plain, WidgetId::NONE);
    }

    #[test]
    fn mapping_chevron_is_hit_at_its_own_center() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.set_context(CardContext::Author);
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        let chevron = panel.mapping_chevron_ids[1].expect("row 1 mappable → chevron");
        let rect = panel
            .mapping_chevron_rect(&tree, "strength")
            .expect("chevron rect");
        let center = Vec2::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
        let hit = tree.hit_test(center);
        assert_eq!(
            hit,
            Some(chevron),
            "hit_test at the chevron center must resolve to the chevron, got {hit:?}"
        );
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
        assert!(panel.mapping_chevron_ids[0].is_none(), "row 0 not mappable");
        let chevron = panel.mapping_chevron_ids[1].expect("generator mappable row → chevron");
        let actions = panel.handle_click(chevron);
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
        assert!(panel.mapping_chevron_ids.iter().all(|id| id.is_none()));
    }

    #[test]
    fn handle_click_toggle() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.toggle_btn_id.unwrap());
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectToggle(0)));
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.chevron_btn_id.unwrap());
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::EffectCollapseToggle(0)));
    }

    #[test]
    fn handle_click_driver_button() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.driver_btn_ids[0].unwrap());
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
        use crate::view::UiParamSlot as ParamSlot;
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

    #[test]
    fn two_mods_share_one_tabbed_drawer() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        // Arm driver + envelope on param 0 → two configs active.
        panel.state.mod_state.driver_expanded[0] = true;
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        // A tab strip appears with one tab per active config, and only the shown
        // config (the stored default, Driver) is built — not both stacked.
        assert_eq!(
            panel.mod_tab_ids[0].len(),
            2,
            "tab strip shows both active configs"
        );
        assert!(
            panel.driver_config_ids[0].is_some(),
            "the shown config (driver) is built"
        );
        assert!(
            panel.envelope_config_ids[0].is_none(),
            "the hidden config is not built (no stacking)"
        );
        // Track overlays still show for every armed mod regardless of the tab.
        assert!(panel.trim_ids[0].is_some(), "driver trim stays on the track");
        assert!(
            panel.target_ids[0].is_some(),
            "envelope target stays on the track"
        );
    }

    #[test]
    fn tabbed_drawer_height_is_one_config_plus_strip_not_the_sum() {
        let mut driver_only = ParamCardPanel::new();
        driver_only.configure(&effect_config());
        driver_only.state.mod_state.driver_expanded[0] = true;
        let driver_h = driver_only.compute_height();

        let mut both = ParamCardPanel::new();
        both.configure(&effect_config());
        both.state.mod_state.driver_expanded[0] = true;
        both.state.mod_state.envelope_expanded[0] = true;
        let both_h = both.compute_height();

        // Two armed = the shown config (driver) + the tab strip — exactly one
        // tab strip taller than driver-only. The old stacking would have added a
        // whole ENV_CONFIG_HEIGHT on top.
        let expected = driver_h + crate::panels::param_slider_shared::MOD_TAB_STRIP_H;
        assert!(
            (both_h - expected).abs() < 0.01,
            "two mods = one config + tab strip, not stacked: both={both_h} expected={expected}"
        );
    }

    #[test]
    fn clicking_a_mod_tab_switches_the_shown_config() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.driver_expanded[0] = true;
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (env_tab_id, _) = panel.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Envelope)
            .copied()
            .expect("envelope tab present");
        let actions = panel.handle_click(env_tab_id);
        assert!(matches!(actions.as_slice(), [PanelAction::ModConfigTabChanged]));
        assert_eq!(panel.mod_active_tab[0], ModTab::Envelope);
    }

    #[test]
    fn reconfigure_preserves_mod_tab_choice() {
        // The bug fix: a re-sync reconfigures the SAME panel (the inspector now
        // reuses it by effect id), so the user's tab choice must survive
        // `configure` instead of resetting to the default and snapping back.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.driver_expanded[0] = true;
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (env_tab_id, _) = panel.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Envelope)
            .copied()
            .expect("envelope tab present");
        panel.handle_click(env_tab_id);
        assert_eq!(panel.mod_active_tab[0], ModTab::Envelope);

        // Re-sync (same effect) → configure must not clobber the tab choice.
        panel.configure(&effect_config());
        assert_eq!(
            panel.mod_active_tab[0],
            ModTab::Envelope,
            "configure reset the tab — the snap-back bug would be back"
        );

        // And the rebuilt drawer shows the envelope config, not the driver.
        panel.state.mod_state.driver_expanded[0] = true;
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
        assert!(panel.envelope_config_ids[0].is_some());
        assert!(panel.driver_config_ids[0].is_none());
    }

    #[test]
    fn effect_header_layout_matches_golden() {
        // The host-built effect header lands toggle / cog / chevron at the right,
        // with the expand chevron always the rightmost control (matches the
        // generator header trailing order).
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new(); // Perform context
        panel.configure(&effect_config());
        let rect = Rect::new(0.0, 0.0, 280.0, 300.0);
        panel.build(&mut tree, rect);

        let inner_x = rect.x + BORDER_W;
        let inner_y = rect.y + BORDER_W;
        let inner_w = rect.width - BORDER_W * 2.0;
        let chevron_x = inner_x + inner_w - PADDING - CHEVRON_W;
        let cog_x = chevron_x - GAP - COG_W;
        let toggle_x = cog_x - GAP - TOGGLE_W;
        let elem_y = inner_y + (HEADER_HEIGHT - 16.0) * 0.5;

        let close = |a: Rect, b: Rect| {
            (a.x - b.x).abs() < 0.01
                && (a.y - b.y).abs() < 0.01
                && (a.width - b.width).abs() < 0.01
                && (a.height - b.height).abs() < 0.01
        };
        let toggle = tree.get_bounds(panel.host.node_id_for_key(KEY_TOGGLE).unwrap());
        assert!(close(toggle, Rect::new(toggle_x, elem_y, TOGGLE_W, 16.0)), "toggle {toggle:?}");
        let chevron = tree.get_bounds(panel.host.node_id_for_key(KEY_CHEVRON).unwrap());
        assert!(close(chevron, Rect::new(chevron_x, elem_y, CHEVRON_W, 16.0)), "chevron {chevron:?}");
        let cog = tree.get_bounds(panel.host.node_id_for_key(KEY_COG).unwrap());
        assert!(close(cog, Rect::new(cog_x, elem_y, COG_W, 16.0)), "cog {cog:?}");
    }

    #[test]
    fn param_label_column_aligns_to_section_inset() {
        // §14.2 rule 1 / §14.5 C — one inset. The effect card and generator card
        // land their first param label on the SAME left column, and that column is
        // the canonical `SECTION_CONTENT_INSET` (= card 1px border + SPACE_M). The
        // border-less chrome panels set `PAD_H = SECTION_CONTENT_INSET`, so they
        // share this column by construction; pinning the card side here guards the
        // whole alignment (it trips if PADDING drifts off SPACE_M or the border
        // changes).
        let label_x = |cfg: &ParamCardConfig| -> f32 {
            let mut tree = UITree::new();
            let mut panel = ParamCardPanel::new();
            panel.configure(cfg);
            panel.set_collapsed(false);
            panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
            panel
                .slider_ids
                .iter()
                .flatten()
                .find_map(|s| s.label)
                .map(|id| tree.get_bounds(id).x)
                .expect("a built param label")
        };
        let ex = label_x(&effect_config());
        let gx = label_x(&gen_config());
        assert!(
            (ex - gx).abs() < 0.01,
            "effect + gen param labels share one column: {ex} vs {gx}"
        );
        assert!(
            (ex - color::SECTION_CONTENT_INSET).abs() < 0.01,
            "param label sits at the canonical section inset: {ex} vs {}",
            color::SECTION_CONTENT_INSET
        );
        assert!(
            (color::SECTION_CONTENT_INSET - (BORDER_W + PADDING)).abs() < 0.01,
            "card content inset = 1px frame border + PADDING (SPACE_M)"
        );
    }

    // ── Generator-card fixtures + tests ───────────────────────────

    #[test]
    fn generator_header_layout_matches_golden() {
        // The host-built generator header must land Change / cog / chevron at the
        // same right-to-left rects the old imperative layout used.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new(); // Perform context by default
        panel.configure(&gen_config());
        let rect = Rect::new(0.0, 0.0, 280.0, 300.0);
        panel.build(&mut tree, rect);

        let inner_x = rect.x + BORDER_W;
        let inner_y = rect.y + BORDER_W;
        let inner_w = rect.width - BORDER_W * 2.0;
        // §14.5 D — trailing controls right-align to the shared `inner_right - PADDING`
        // gutter (was flush to the inner edge).
        let chevron_x = inner_x + inner_w - PADDING - CHEVRON_W;
        let cog_x = chevron_x - COG_W;
        let change_x = cog_x - CHANGE_BTN_W - GAP;

        let close = |a: Rect, b: Rect| {
            (a.x - b.x).abs() < 0.01
                && (a.y - b.y).abs() < 0.01
                && (a.width - b.width).abs() < 0.01
                && (a.height - b.height).abs() < 0.01
        };
        let chevron = tree.get_bounds(panel.host.node_id_for_key(KEY_CHEVRON).unwrap());
        assert!(
            close(chevron, Rect::new(chevron_x, inner_y, CHEVRON_W, HEADER_HEIGHT)),
            "chevron {chevron:?}"
        );
        let cog = tree.get_bounds(panel.host.node_id_for_key(KEY_COG).unwrap());
        assert!(close(cog, Rect::new(cog_x, inner_y, COG_W, HEADER_HEIGHT)), "cog {cog:?}");
        let change = tree.get_bounds(panel.host.node_id_for_key(KEY_CHANGE).unwrap());
        assert!(
            close(
                change,
                Rect::new(
                    change_x,
                    inner_y + (HEADER_HEIGHT - CHANGE_BTN_H) * 0.5,
                    CHANGE_BTN_W,
                    CHANGE_BTN_H
                )
            ),
            "change {change:?}"
        );
    }

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
            driver_free_period: vec![None; 3],
            audio: Default::default(),
        }
    }

    #[test]
    fn build_gen_param() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.border_id.is_some());
        assert!(panel.name_label_id.is_some());
        assert!(panel.chevron_btn_id.is_some());
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
        let actions = panel.handle_click(panel.change_btn_id.unwrap());
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::GenTypeClicked(_)));

        // Clicking the name label selects the card
        let actions = panel.handle_click(panel.name_label_id.unwrap());
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::GenCardClicked));
    }

    #[test]
    fn handle_click_toggle_param() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let button_id = panel.toggle_ids[1].as_ref().unwrap().button_id;
        let actions = panel.handle_click(button_id);
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
                crate::view::UiParamSlot::exposed(5.0),
                crate::view::UiParamSlot::exposed(1.0),
                crate::view::UiParamSlot::exposed(2.5),
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
        assert!((expanded_h - base_h - driver_config_height()).abs() < 0.1);
    }

    #[test]
    fn compact_mode_hides_driver_drawer_height() {
        // §6b: with a driver armed, compact mode drops the drawer back out of the
        // card height (mod stays armed; only the config drawer is hidden).
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        let base_h = panel.compute_height();
        panel.state.mod_state.driver_expanded[0] = true;
        let expanded_h = panel.compute_height();
        assert!(expanded_h > base_h, "armed driver should add the drawer height");

        panel.set_compact(true);
        let compact_h = panel.compute_height();
        assert!(
            (compact_h - base_h).abs() < 0.1,
            "compact should hide the drawer: compact={compact_h} base={base_h}"
        );
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
