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
//! `has_graph_mod`; generator-only: `string_params`). `is_toggle`/
//! `is_trigger` apply to both kinds (§8.4 P3b gave effect cards the same
//! toggle/trigger row rendering generators already had — see
//! `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md` §8). Readers branch on
//! [`ParamCardKind`] or ignore the field that doesn't apply to them.

use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_slider_shared::*;
use super::{AudioShapeParam, GraphParamTarget, PanelAction, TrimKind, UiRelightField, UiRelightHeightFrom};
use crate::anim::{AnimF32, Transient};
use crate::chrome::{Align, ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds, TrackSpan};
use crate::transform2d::Affine2;
use crate::tree::UITree;
use manifold_foundation::{EffectId, LayerId, RELIGHT_FEATURE_ENABLED};

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
/// The "3D Shading" header icon (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5b) —
/// sits right of the ON/OFF toggle, left of the cog, in both card kinds.
/// Unconditional in both `CardContext`s (like the ON/OFF toggle) — it's a
/// real per-instance flag, not editor-only chrome.
const KEY_RELIGHT: u64 = 90_011;

/// D1 tab-ink slide: height of the sliding underline beneath the mod-config
/// tab strip, inset from the tab's own bottom edge (`HAIRLINE_RADIUS`-scale —
/// a crisp accent line, not a filled bar).
const MOD_TAB_INK_H: f32 = 2.0;

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

/// The D3 relight-knob rows' colors when the "3D Shading" toggle is off —
/// desaturated off the grey ramp instead of the blue accent, so the greyed
/// state reads as visually distinct from an armed slider (no-conditionally-
/// visible-ui: still interactive, just not the "live" look).
fn relight_disabled_slider_colors() -> SliderColors {
    SliderColors {
        track: color::SLIDER_TRACK_C32,
        track_hover: color::SLIDER_TRACK_C32,
        track_pressed: color::SLIDER_TRACK_C32,
        fill: color::BG_3_HOVER,
        thumb: color::TEXT_DIMMED_C32,
        text: color::TEXT_DIMMED_C32,
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
    /// This is the outer-card gate for a generator's/effect's audio trigger
    /// response (the `clip_trigger` toggle on the 11 trigger-responsive
    /// generators and Strobe). Always paired with `is_toggle: true,
    /// is_trigger: false` — a toggle row that additionally reaches the
    /// standard per-param audio-mod "A" drawer (§9) instead of the plain
    /// zero-lane toggle. See `docs/LIVE_AUDIO_TRIGGERS_DESIGN.md` §9.
    pub is_trigger_gate: bool,
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
    /// Card-bundling section name (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2
    /// D5). Contiguous runs of rows sharing the same `Some(name)` draw under
    /// one collapsible header; `None` rows render exactly as today (a flat
    /// slider, no header). Comes straight off the manifest spec — never
    /// derived from graph structure here.
    pub section: Option<String>,
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

/// Config for the "3D Shading" toggle + D3 knobs (`docs/DEPTH_RELIGHT_DESIGN.md`
/// P5b) — the union `ParamCardConfig` carries for both effect and generator
/// cards. Always present (mirrors `PresetInstance.relight`/`relight_params`
/// always being live on the instance): the card renders the six knobs +
/// Height From row greyed rather than hidden when `enabled` is false
/// (no-conditionally-visible-ui), so the values must survive a
/// toggle-off/toggle-on round trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelightCardConfig {
    pub enabled: bool,
    pub light_x: f32,
    pub light_y: f32,
    pub relief: f32,
    pub ao_intensity: f32,
    pub shadow_softness: f32,
    pub gain: f32,
    pub height_from: UiRelightHeightFrom,
}

/// One D3 knob's static shape — label + clamp range + reset default. The
/// SINGLE source both `build_relight_rows` (rendering) and the drag hit-test
/// in `handle_pointer_down` read, so the two can't drift out of range.
struct RelightFieldSpec {
    field: UiRelightField,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
}

/// D3's proven ranges, in `RelightField` declaration order — mirror the
/// underlying atoms' own `ParamDef` ranges (`lambert_directional`/
/// `heightfield_shadow`'s light_x/y, `ssao_gtao`'s relief/intensity,
/// `heightfield_shadow`'s softness, `node.gain`'s gain). `ui` cannot read the
/// registry directly (`RelightCardConfig`'s doc), so these are pinned here.
const RELIGHT_FIELD_SPECS: [RelightFieldSpec; 6] = [
    RelightFieldSpec { field: UiRelightField::LightX, label: "Light X", min: -1.0, max: 1.0, default: 0.4 },
    RelightFieldSpec { field: UiRelightField::LightY, label: "Light Y", min: -1.0, max: 1.0, default: 0.6 },
    RelightFieldSpec { field: UiRelightField::Relief, label: "Relief", min: 0.01, max: 2.0, default: 0.25 },
    RelightFieldSpec {
        field: UiRelightField::AoIntensity,
        label: "AO Intensity",
        min: 0.0,
        max: 4.0,
        default: 1.3,
    },
    RelightFieldSpec {
        field: UiRelightField::ShadowSoftness,
        label: "Shadow Softness",
        min: 0.0,
        max: 1.0,
        default: 0.5,
    },
    RelightFieldSpec { field: UiRelightField::Gain, label: "Gain", min: 0.0, max: 4.0, default: 1.4 },
];

impl RelightCardConfig {
    /// Read one knob's current value by field — the single accessor the
    /// row-drag path uses so it never has to match on `RelightField` itself.
    fn value(&self, field: UiRelightField) -> f32 {
        match field {
            UiRelightField::LightX => self.light_x,
            UiRelightField::LightY => self.light_y,
            UiRelightField::Relief => self.relief,
            UiRelightField::AoIntensity => self.ao_intensity,
            UiRelightField::ShadowSoftness => self.shadow_softness,
            UiRelightField::Gain => self.gain,
        }
    }

    /// Live-preview write for a mid-drag update (never committed here — the
    /// app layer owns the undo-tracked commit on release, mirroring every
    /// other slider row).
    fn set_value(&mut self, field: UiRelightField, value: f32) {
        match field {
            UiRelightField::LightX => self.light_x = value,
            UiRelightField::LightY => self.light_y = value,
            UiRelightField::Relief => self.relief = value,
            UiRelightField::AoIntensity => self.ao_intensity = value,
            UiRelightField::ShadowSoftness => self.shadow_softness = value,
            UiRelightField::Gain => self.gain = value,
        }
    }
}

impl Default for RelightCardConfig {
    /// D3's proven v6 recipe defaults — mirrors
    /// `manifold_core::effects::RelightParams::default()` field-for-field.
    /// `ui` cannot depend on `manifold-core`; kept in sync by
    /// `preset_to_config`'s (manifold-app) doc comment pointing back here.
    fn default() -> Self {
        Self {
            enabled: false,
            light_x: 0.4,
            light_y: 0.6,
            relief: 0.25,
            ao_intensity: 1.3,
            shadow_softness: 0.5,
            gain: 1.4,
            height_from: UiRelightHeightFrom::Auto,
        }
    }
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
    /// list). Bundled so the config grows by one field. An `is_trigger_gate`
    /// row's config rides this SAME state (§9 — a trigger-gate card's audio
    /// config is a normal `ParameterAudioMod`, not a separate per-instance
    /// field); `trigger_mode_idx` is the one extra piece it reads.
    pub audio: super::param_slider_shared::AudioCardState,
    /// Per-param: an enabled automation lane (≥1 point) exists on this
    /// instance for this param — drives the red "automated" dot (P4 §7).
    pub automation_active: Vec<bool>,
    /// Per-param: that lane is currently overridden (latched) — the dot
    /// grays instead of showing red.
    pub automation_overridden: Vec<bool>,
    /// The "3D Shading" toggle + D3 knobs (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// P5b) — see [`RelightCardConfig`]'s doc.
    pub relight: RelightCardConfig,
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
/// Card title font — the heading token, larger than the body-size param rows so
/// the effect/generator name reads as a title, not another parameter.
const HEADER_FONT_SIZE: u16 = color::FONT_HEADING;
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
/// Width of the right-edge mapping-drawer chevron lane. Rows shrink their
/// slider by this much so the chevron sits past the D/E buttons at the row's
/// right edge.
const MAP_CHEVRON_W: f32 = 14.0;

/// A row's label/slider/chevron-lane geometry for a given available content
/// width — the single source both `build_effect_sliders` and
/// `build_generator` consume instead of each doing their own lane arithmetic
/// inline (`GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4, D2: "one
/// row-geometry helper... no builder does lane arithmetic inline"). The
/// chevron lane is reserved only in `CardContext::Author`, where the glyph
/// can actually draw (BUG-160 follow-up: reserving it in `Perform` too, per
/// the original D1, shrank the timeline inspector's sliders for a control
/// that never appears there — Peter's directive was "mapping chevron the
/// only extra" in the editor, not a lane Perform pays for and never uses).
pub(crate) struct RowGeometry {
    /// Width of the param-name label column.
    pub(crate) label_width: f32,
    /// Width of the draggable slider track (content width minus the D/E/A
    /// button lane, and the chevron lane when reserved).
    pub(crate) slider_w: f32,
}

pub(crate) fn row_geometry(content_w: f32, reserve_chevron: bool) -> RowGeometry {
    let chevron_lane = if reserve_chevron { MAP_CHEVRON_W + DE_BUTTON_GAP } else { 0.0 };
    let label_width = crate::slider::label_width_for_row(content_w);
    let slider_w =
        content_w - MOD_LANE_GAP - DE_BUTTON_SIZE * 3.0 - DE_BUTTON_GAP * 2.0 - chevron_lane;
    RowGeometry { label_width, slider_w }
}

// Effect shell furniture.
const DRAG_HANDLE_W: f32 = 18.0;
const TOGGLE_W: f32 = 30.0;
/// Width of the "3D Shading" header icon — a short "3D" glyph, narrower than
/// the ON/OFF toggle (no "OFF"-length text to fit).
const RELIGHT_W: f32 = 24.0;
// 3-letter chips (ABL/ENV/DRV/MOD) at FONT_CAPTION don't need 36px; the
// narrower chip reclaims header width for the effect name when several show.
const BADGE_W: f32 = 28.0;
const BADGE_H: f32 = 14.0;
const BADGE_RADIUS: f32 = 7.0;
const CONFIG_BTN_FONT_SIZE: u16 = color::FONT_CAPTION;

// Generator shell furniture.
const CHANGE_BTN_W: f32 = 60.0;
const CHANGE_BTN_H: f32 = 16.0;

// ── Internal node ID structs ─────────────────────────────────────
//
// `TOGGLE_BTN_W`/`TOGGLE_BTN_H`/`ToggleParamIds` moved to
// `param_slider_shared` (`build_toggle_trigger_row`) — shared by both card
// kinds now that effects build toggle/trigger rows too.

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
    /// P2 "card collapse" (`UI_CRAFT_AND_MOTION_PLAN.md` D17) — eases the
    /// card's body height between 0 (collapsed) and 1 (expanded) instead of
    /// snapping, so cards below reflow smoothly (same "AnimF32 drives
    /// `compute_height`" technique as `drawer_height_anim`, one level up:
    /// the whole card body instead of one param's drawer). Effect cards
    /// only — `build_generator`'s rows parent flat to root (`None`) rather
    /// than threading a `parent` `NodeId` the way `build_effect_sliders`
    /// does, so there's no reparent-to-a-clip-region seam to hook a
    /// mid-flight reveal into without a much larger per-row rewrite;
    /// generator cards keep the instant collapse `compute_height_generator`
    /// always had. See `collapse_frac`/`sync_collapse_anim`.
    collapse_anim: AnimF32,
    /// Whether `collapse_anim` has been pointed at a real target at least
    /// once. `false` only before the very first `configure()` — a freshly
    /// constructed card SNAPS to its initial state (no slide-in on first
    /// appearance), matching `drawer_height_anim`'s `prev_anim_len == 0`
    /// convention.
    collapse_configured: bool,
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
    /// The "3D Shading" header icon (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5b) —
    /// shared shell, present on both card kinds.
    relight_btn_id: Option<NodeId>,
    /// D3/D4 relight card state — always present (see `RelightCardConfig`'s
    /// doc), the source the always-visible-but-greyed rows below the normal
    /// params read/write.
    relight: RelightCardConfig,
    /// The six D3 knob rows' slider ids, in `RelightField` declaration order
    /// (Light X, Light Y, Relief, AO Intensity, Shadow Softness, Gain).
    relight_slider_ids: [Option<crate::slider::SliderNodeIds>; 6],
    /// The matching right-click reset action for each relight slider, same
    /// order — mirrors `slider_resets`, registered in `register_intents`.
    relight_slider_resets: [Option<PanelAction>; 6],
    /// D4 "Height From" enum row — one button per option (Auto / Luminance /
    /// Inverted Luminance), the active one tinted.
    relight_height_btn_ids: [Option<NodeId>; 3],

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
    /// Per-param right-click reset for `slider_ids[pi]`'s track — a parallel
    /// array (rather than folding into `slider_ids`) so the many existing
    /// `slider_ids[pi].track`/`.value_text`/etc. access sites are untouched.
    /// `Some` exactly when `slider_ids[pi]` is `Some` (BUG-070 follow-through).
    slider_resets: Vec<Option<PanelAction>>,
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
    /// Per-param audio drawer ids + send count (for click resolution). An
    /// `is_trigger_gate` row's "A" button + drawer live here too (§9) —
    /// same mechanism as any other audio mod.
    audio_configs: Vec<Option<(crate::panels::drawer::DrawerIds, usize)>>,
    /// Per-param collapsed-row mode-indicator label (§9, carried over from §8
    /// D6 — shown even when the drawer is closed, `is_trigger_gate` rows
    /// only).
    audio_trigger_mode_badge_ids: Vec<Option<NodeId>>,
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
    /// Per-param drawer open/close height tween (`UI_CRAFT_AND_MOTION_PLAN.md`
    /// P1). Eases toward `row_drawer_height(i)` so arming/disarming a modulator
    /// grows/shrinks the drawer and reflows content below. UI-only state, preserved
    /// across rebuilds like `mod_active_tab` (configure resizes without clobbering,
    /// so a forced per-frame rebuild mid-tween doesn't reset it). Ticked only for
    /// `Perform` (inspector) cards; `Author` (graph-editor) cards snap, since
    /// nothing drives their tick — see `configure`.
    drawer_height_anim: Vec<AnimF32>,
    /// Per-param modulation-config tab strip node ids paired with their `ModTab`,
    /// for routing tab clicks. Empty for rows with fewer than two active configs.
    /// Rebuilt each frame.
    mod_tab_ids: Vec<Vec<(NodeId, ModTab)>>,
    /// D1 "tab-ink slide" (`UI_CRAFT_AND_MOTION_PLAN.md` P2) — per-param x-position
    /// tween for the sliding underline beneath the mod-config tab strip
    /// (`Trigger`/`LFO`/`Audio`/`Ableton`). Preserved across rebuilds like
    /// `mod_active_tab`/`drawer_height_anim`; ticked by the same `tick_drawers`
    /// rail, so switching tabs needs no new app-side poll. `target() == 0.0 &&
    /// value() == 0.0` (the fresh `AnimF32::new` state) doubles as "never
    /// positioned yet" — snap instead of ease so a tab strip appearing for the
    /// first time doesn't visibly slide in from x=0.
    mod_tab_ink: Vec<AnimF32>,
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

    /// D5 card-section fold state (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md
    /// §2), keyed by section name. UI-local workspace state — same home as
    /// the graph canvas's `GraphCanvas::collapsed` (survives rebuilds of
    /// this panel instance, never serialized to the project; folds reset on
    /// app restart, persistence Deferred). Missing entry = unfolded
    /// (default). A section not present in the current `param_info` is
    /// simply never consulted — no pruning needed.
    section_folded: ahash::AHashMap<String, bool>,
    /// Rebuilt every build pass: `(header_node_id, section_name)` for every
    /// section-header row drawn this frame, so `handle_click` can resolve a
    /// header click back to its section without a second id → name map.
    section_header_ids: Vec<(NodeId, String)>,

    copied_flash: CopyToClipboardLabelState,

    // Drag state
    drag: ParamDragState,

    // Caches (NaN = needs sync)
    param_cache: Vec<f32>,
    toggle_cache: Vec<bool>,
    label_cache: Vec<Option<String>>,
    /// P2 "value-change flash" (`UI_CRAFT_AND_MOTION_PLAN.md` §4) — per-param
    /// one-shot fired when `sync_values` sees a genuine value change (not the
    /// post-`configure()` NaN resync, and not while this card's slider is being
    /// dragged — the drag itself is the feedback). Ticked alongside
    /// `drawer_height_anim` in `tick_drawers`; ties the value-text color back to
    /// normal once `progress()` returns `None`.
    value_flash: Vec<Transient>,

    /// P2 "value snap-back" (D15) — per-param `AnimF32` (`Curve::Snap`) that
    /// eases the slider FILL from its pre-reset position to the default after
    /// the RIGHT-CLICK reset gesture (`begin_value_snapback`, meant to be
    /// called by the app-side dispatch once the model has already snapped
    /// instantly — D15: data first, visual follows). BUG-061 folded the
    /// param right-click reset into the generic `SliderReset` trio, which
    /// reuses the plain `ParamChanged` handler and does not call this — no
    /// production path drives it today; only its own unit tests below do.
    /// Settled (not animating) for every row until a reset targets it. Ticked in
    /// `tick_value_flash` alongside the flash, which also repaints the fill
    /// every frame it's mid-flight (the value-dirty-check in `sync_values`
    /// only fires once, the frame the model value actually changes).
    value_snapback: Vec<AnimF32>,

    /// D17 "spawn pop" — a brand-new card (no existing panel matched in
    /// `InspectorCompositePanel::reconcile_cards`) enters at scale 0.94→1
    /// eased with `Curve::Snap`. A reused card stays settled at `1.0`
    /// (`AnimF32::new(1.0, ..)`'s default), so an ordinary reconfigure never
    /// re-pops. Applied as an already-scaled outer rect (pivot = card
    /// center) at the top of `build_effect`/`build_generator` — every child
    /// node built from the resulting (smaller, host-computed) `inner` bounds
    /// follows along, so the whole card pops as one rigid piece without a
    /// general transform stack.
    spawn_scale: AnimF32,
    /// D17 "delete collapse" (exit-state pattern, `anim.rs`'s doc comment) —
    /// `Some` only while `InspectorCompositePanel`'s `dying` list is still
    /// drawing this card after it was removed from the model. Drives
    /// `is_delete_finished`; the collapse itself rides the existing
    /// `collapse_anim` mechanism (`begin_delete_collapse` retargets it to 0).
    delete_fade: Option<Transient>,

    // Node range
    first_node: usize,
    node_count: usize,

    // Card position (for effect drag-reorder hit testing)
    card_y: f32,
}

/// D17 "spawn pop" geometry: the card's outer frame rect scaled by `s` about
/// its own center — `(x, y)` is the card's UNSCALED top-left, `(w, h)` its
/// UNSCALED width/height. A no-op (returns the exact input rect) once `s`
/// settles at `1.0`, so a settled card's geometry is bit-identical to the
/// pre-motion path.
fn scaled_card_rect(x: f32, y: f32, w: f32, h: f32, s: f32) -> Rect {
    if (s - 1.0).abs() < 0.0005 {
        return Rect::new(x, y, w, h);
    }
    let cx = x + w * 0.5;
    let cy = y + h * 0.5;
    Rect::new(cx - w * 0.5 * s, cy - h * 0.5 * s, w * s, h * s)
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
            collapse_anim: AnimF32::new(1.0, color::MOTION_MED_MS),
            collapse_configured: false,
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
            relight_btn_id: None,
            relight: RelightCardConfig::default(),
            relight_slider_ids: [None; 6],
            relight_slider_resets: std::array::from_fn(|_| None),
            relight_height_btn_ids: [None; 3],
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
            slider_resets: Vec::new(),
            base_values: Vec::new(),
            row_catcher_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            audio_btn_ids: Vec::new(),
            audio_configs: Vec::new(),
            audio_trigger_mode_badge_ids: Vec::new(),
            target_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            trim_ids: Vec::new(),
            ableton_trim_ids: Vec::new(),
            audio_trim_ids: Vec::new(),
            ableton_config_ids: Vec::new(),
            mod_active_tab: Vec::new(),
            drawer_height_anim: Vec::new(),
            mod_tab_ids: Vec::new(),
            mod_tab_ink: Vec::new(),
            compact: false,
            toggle_ids: Vec::new(),
            string_param_btn_ids: Vec::new(),
            mapping_chevron_ids: Vec::new(),
            osc_addresses: Vec::new(),
            section_folded: ahash::AHashMap::new(),
            section_header_ids: Vec::new(),
            copied_flash: CopyToClipboardLabelState::default(),
            drag: ParamDragState::new(),
            param_cache: Vec::new(),
            toggle_cache: Vec::new(),
            label_cache: Vec::new(),
            value_flash: Vec::new(),
            value_snapback: Vec::new(),
            spawn_scale: AnimF32::new(1.0, color::MOTION_MED_MS),
            delete_fade: None,
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
        self.relight = config.relight;
        self.is_collapsed = config.collapsed;
        self.sync_collapse_anim();
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
            &config.automation_active,
            &config.automation_overridden,
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
        self.slider_resets = vec![None; n];
        self.base_values = vec![0.0; n];
        self.row_catcher_ids = vec![None; n];
        self.driver_btn_ids = vec![None; n];
        self.envelope_btn_ids = vec![None; n];
        self.driver_config_ids = Vec::new();
        self.driver_config_ids.resize_with(n, || None);
        self.audio_btn_ids = vec![None; n];
        self.audio_configs = Vec::new();
        self.audio_configs.resize_with(n, || None);
        self.audio_trigger_mode_badge_ids = vec![None; n];
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
        // P1 drawer tween targets. Preserve existing tweens across the rebuild (a
        // mid-flight tween must not reset), grow for new params. Then point each at
        // its settled drawer height: a *new* param snaps so it never stalls
        // half-open; an existing param eases (set_target no-ops when the target is
        // unchanged, so the per-frame rebuild that drives the tween doesn't reset
        // it). Targets are read into a temp first — `row_drawer_height` borrows
        // `&self` while the loop needs `&mut self.drawer_height_anim`.
        //
        // Both contexts ease identically since
        // `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4 (D4): the editor's
        // `UIRoot` now ticks its inspector every frame it presents
        // (`UIRoot::tick_inspector`), so an Author card's tween advances the
        // same as a Perform card's — the old never-ticked-Author snap
        // workaround is gone because the workaround is.
        let prev_anim_len = self.drawer_height_anim.len();
        self.drawer_height_anim
            .resize_with(n, || AnimF32::new(0.0, color::MOTION_MED_MS));
        self.drawer_height_anim.truncate(n);
        let drawer_targets: Vec<f32> = (0..n).map(|i| self.row_drawer_height(i)).collect();
        for (i, &target) in drawer_targets.iter().enumerate() {
            if i < prev_anim_len {
                self.drawer_height_anim[i].set_target(target);
            } else {
                self.drawer_height_anim[i].snap(target);
            }
        }
        self.mod_tab_ids = vec![Vec::new(); n];
        // Ink x-position targets are only knowable once the tab strip is laid
        // out (build time, not here) — resize only; `sync_mod_tab_ink` sets
        // targets per-row after `build_param_row` returns.
        self.mod_tab_ink.resize_with(n, || AnimF32::new(0.0, color::MOTION_MED_MS));
        self.mod_tab_ink.truncate(n);
        self.toggle_ids = Vec::new();
        self.toggle_ids.resize_with(n, || None);
        self.mapping_chevron_ids = vec![None; n];
        self.string_param_btn_ids = vec![None; config.string_params.len()];
        self.param_cache = vec![f32::NAN; n];
        self.toggle_cache = vec![false; n];
        self.label_cache = vec![None; n];
        self.value_flash.resize_with(n, Transient::default);
        self.value_flash.truncate(n);
        self.value_snapback
            .resize_with(n, || AnimF32::new(0.0, color::MOTION_MED_MS).with_curve(crate::anim::Curve::Snap));
        self.value_snapback.truncate(n);
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
    ///
    /// Called only from `route_value_typein` on `UIEvent::DoubleClick`
    /// (inspector.rs:2375) — this IS the contract's `(ValueCell, DoubleClick)
    /// -> EditValue` row (D13/D14), constructed at input time because the
    /// action's payload (anchor, value, clamp range) is live state, not a
    /// build-time constant (D14). The debug_assert below is the single
    /// written record that this call site and the contract table agree.
    pub fn value_cell_typein(&self, node_id: NodeId, tree: &UITree) -> Option<PanelAction> {
        debug_assert_eq!(
            crate::slider::BitmapSlider::intent_for(
                crate::slider::SliderZone::ValueCell,
                crate::intent::Gesture::DoubleClick
            ),
            Some(crate::slider::SliderIntent::EditValue),
            "value_cell_typein is the contract's ValueCell+DoubleClick->EditValue translation (D13/D14)"
        );
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
    /// D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
    /// P3c, BUG-082's fix; widened 2026-07-11): push this tick's live
    /// shaped-signal level onto every OPEN row's Amount meter — in place, no
    /// rebuild. Every enabled audio-mod row now captures a level on the
    /// content-thread side (`evaluate_instance_audio_mods`), not just
    /// `is_trigger_gate` ones, so this no longer filters by
    /// `info.is_trigger_gate` — a continuous/Step/Random drawer's meter now
    /// updates exactly like a trigger-gate drawer's. Keyed on
    /// `(effect_id, param_id)` via `manifold_foundation::
    /// fire_meter_key_for_param` — the SAME constructor the content-thread
    /// capture uses — so `fire_level` (built at the app boundary from
    /// `ContentState::fire_meters`, a `manifold-core` type `manifold-ui`
    /// cannot depend on — `docs/UI_LAYERING_INVERSION.md`) resolves it.
    /// Amount is always the first `Slider` row `build_audio_mod_drawer`
    /// builds (`DrawerIds::meters[0]`), regardless of which trailing rows
    /// follow. `dt` (BUG-109 P5) is the UI frame delta seconds, threaded down
    /// to [`crate::panels::drawer::MeterIds::update`] for its peak-hold
    /// timing.
    pub fn update_fire_meters(
        &self,
        tree: &mut UITree,
        fire_level: &dyn Fn(u64) -> Option<f32>,
        dt: f32,
    ) {
        for (pi, cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, _)) = cfg else { continue };
            let Some(info) = self.param_info.get(pi) else { continue };
            let Some(Some(meter)) = dids.meters.first() else { continue };
            let key = manifold_foundation::fire_meter_key_for_param(
                self.effect_id.as_str(),
                info.param_id.as_ref(),
            );
            let level = fire_level(key).unwrap_or(0.0);
            meter.update(tree, level, AUDIO_MOD_ACTIVE_C32, dt);
        }
    }

    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 5):
    /// param index of the currently-OPEN fire-mode (`is_trigger_gate`, armed)
    /// drawer, if any — deliberately narrower than the Amount meter itself
    /// (every open drawer shows a meter, 2026-07-11): only a fire-mode config
    /// re-taps the scope send/band here. A plain continuous mod's open drawer
    /// never matches. First match wins; a card with two armed trigger-gate
    /// rows is not a case this app produces today.
    fn open_fire_mode_drawer_row(&self) -> Option<usize> {
        self.audio_configs.iter().enumerate().find_map(|(pi, cfg)| {
            cfg.as_ref()?;
            let info = self.param_info.get(pi)?;
            info.is_trigger_gate.then_some(pi)
        })
    }

    /// The send the currently-open fire-mode drawer is reading, if any.
    pub fn open_fire_mode_drawer_send(&self) -> Option<manifold_foundation::AudioSendId> {
        let pi = self.open_fire_mode_drawer_row()?;
        let idx = self.state.mod_state.audio_send_idx.get(pi).copied().unwrap_or(-1);
        if idx < 0 {
            return None;
        }
        self.state.mod_state.audio_send_ids.get(idx as usize).cloned()
    }

    /// The band the currently-open fire-mode drawer is reading, if any.
    pub fn open_fire_mode_drawer_band(&self) -> Option<crate::types::AudioBand> {
        let pi = self.open_fire_mode_drawer_row()?;
        let idx = self.state.mod_state.audio_band_idx.get(pi).copied().unwrap_or(0);
        crate::types::AudioBand::ALL.get(idx as usize).copied()
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

    /// Whether the D17 "spawn pop" scale-in is still in flight. Exposed for
    /// `InspectorCompositePanel`'s `reconcile_cards` tests (in a different
    /// module — `spawn_scale` itself stays private).
    pub fn is_spawning(&self) -> bool {
        self.spawn_scale.is_animating()
    }

    /// Whether the D17 "card collapse" tween is still in flight. Same
    /// cross-module test-accessor purpose as `is_spawning`.
    pub fn is_collapse_animating(&self) -> bool {
        self.collapse_anim.is_animating()
    }
    /// Force the collapsed flag directly, snapping `collapse_anim` (no ease).
    /// This is the "test/automation harness drives it directly" setter
    /// (mirrors `chevron_node_id`'s doc comment) — production code toggles
    /// collapse through the model (`fx.collapsed` /
    /// `PanelAction::EffectCollapseToggle`) and `configure()`'s
    /// `sync_collapse_anim`, which eases. The one production caller of this
    /// setter is the generator param panel (`ui_bridge/inspector.rs`), whose
    /// `build_generator` can't render a partial-height body anyway (see
    /// `collapse_anim`'s doc comment) — always-snap is correct for it too.
    pub fn set_collapsed(&mut self, collapsed: bool) {
        self.is_collapsed = collapsed;
        let target = if collapsed { 0.0 } else { 1.0 };
        self.collapse_anim.snap(target);
        self.collapse_configured = true;
    }

    /// Point `collapse_anim` at the target implied by `is_collapsed`/`kind`.
    /// Called from `configure()` — the real per-rebuild path a model-driven
    /// collapse toggle (`PanelAction::EffectCollapseToggle`) round-trips
    /// through. Effect cards ease once already configured once (mirrors
    /// `drawer_height_anim`'s "don't slide in on first appearance" rule,
    /// and — since `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4 (D4) —
    /// identically in both contexts, now that the editor ticks its
    /// inspector every frame); every other case (first-ever configure, or a
    /// Generator card whose `build_generator` can't render a partial-height
    /// body) snaps instantly so `compute_height`/`build` never disagree.
    fn sync_collapse_anim(&mut self) {
        let target = if self.is_collapsed { 0.0 } else { 1.0 };
        let eases = self.kind == ParamCardKind::Effect && self.collapse_configured;
        if eases {
            self.collapse_anim.set_target(target);
        } else {
            self.collapse_anim.snap(target);
        }
        self.collapse_configured = true;
    }

    /// D17 "card collapse" fraction: `1.0` fully expanded, `0.0` fully
    /// collapsed, eased between by `collapse_anim` for Effect cards.
    /// Generator cards always read the settled boolean (see `collapse_anim`'s
    /// doc comment) — `sync_collapse_anim` snaps them, so this is exactly
    /// `0.0`/`1.0` there too, just never mid-flight.
    fn collapse_frac(&self) -> f32 {
        self.collapse_anim.value().clamp(0.0, 1.0)
    }

    /// P2 "caret rotate" — maps `collapse_frac()` (reusing `collapse_anim`,
    /// NOT a second animation clock) onto the down-pointing chevron glyph's
    /// rotation: expanded (`frac == 1.0`) sits at 0° (▼), collapsed
    /// (`frac == 0.0`) rotates to -90° (▶, "closing"). Applied via
    /// `UIStyle.transform` (`docs/UI_TRANSFORM_STACK_DESIGN.md`), which
    /// pivots about the chevron node's own rect center — no manual pivot
    /// math here, no glyph swap.
    fn chevron_angle(&self) -> f32 {
        (self.collapse_frac() - 1.0) * std::f32::consts::FRAC_PI_2
    }

    /// D17 "spawn pop" — call once, right after the first `configure()` on a
    /// truly new panel (`InspectorCompositePanel::reconcile_cards`, the only
    /// caller). Restarts `spawn_scale` from 0.94 easing to 1.0 with the
    /// magnetic-snap back-out curve (D15's `Curve::Snap`).
    pub fn fire_spawn_pop(&mut self) {
        self.spawn_scale = AnimF32::new(0.94, color::MOTION_MED_MS).with_curve(crate::anim::Curve::Snap);
        self.spawn_scale.set_target(1.0);
    }

    /// D17 "delete collapse" (exit-state pattern) — call once when this card
    /// has just been dropped from the model's effect list
    /// (`InspectorCompositePanel::reconcile_cards` moves it into a
    /// panel-owned `dying` list instead of discarding it here). Retargets the
    /// existing card-collapse mechanism to fully collapsed (reflows whatever
    /// follows it, exactly like a user-triggered collapse) and starts the
    /// exit fade timer `is_delete_finished` reads.
    pub fn begin_delete_collapse(&mut self) {
        self.collapse_anim.set_target(0.0);
        let mut fade = Transient::default();
        fade.fire(color::MOTION_MED_MS);
        self.delete_fade = Some(fade);
    }

    /// Whether this dying card's exit animation has fully played out — both
    /// the fade timer AND the height collapse have settled. The caller
    /// (`InspectorCompositePanel`'s `dying` list) drops the panel for good
    /// once this is `true`; until then it keeps calling `tick_drawers`/
    /// `build` on it every frame, same as any live card.
    pub fn is_delete_finished(&self) -> bool {
        self.delete_fade
            .as_ref()
            .is_some_and(|f| f.progress().is_none())
            && !self.collapse_anim.is_animating()
    }

    /// P2 "value snap-back" (D15): meant to be called by the app-side dispatch
    /// the instant the RIGHT-CLICK reset-to-default gesture commits — the
    /// model has ALREADY snapped to `to` (raw param value) by the time this
    /// runs; this only retargets the row's `value_snapback` `AnimF32`
    /// (Curve::Snap) so the slider FILL eases from `from` to `to` instead of
    /// jumping — the data is never delayed behind this. BUG-061 folded the
    /// param right-click reset into the generic `SliderReset` trio (plain
    /// `ParamChanged`, no easing) — no production call site remains; kept
    /// for its own unit tests below. A no-op if `param_id` isn't one of this
    /// card's rows (stale/mismatched target).
    pub fn begin_value_snapback(&mut self, param_id: &manifold_foundation::ParamId, from: f32, to: f32) {
        let Some(pi) = self.param_info.iter().position(|p| &p.param_id == param_id) else {
            return;
        };
        let Some(info) = self.param_info.get(pi) else {
            return;
        };
        let from_norm = BitmapSlider::value_to_normalized(from, info.min, info.max);
        let to_norm = BitmapSlider::value_to_normalized(to, info.min, info.max);
        if let Some(anim) = self.value_snapback.get_mut(pi) {
            anim.snap(from_norm);
            anim.set_target(to_norm);
        }
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
    /// Whether this card's targeted instance has diverged from its library
    /// entry (`PresetInstance.graph.is_some()` — the same bit that drives
    /// the MOD badge). Read by the card context menu
    /// (PRESET_LIBRARY_DESIGN P4) to gate Revert/Push to Library — mirrors
    /// [`Self::param_has_ableton_mapping`]'s read-only accessor style.
    pub fn has_graph_mod(&self) -> bool {
        self.state.has_graph_mod
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

    /// Screen-space rect of param row `param_id`'s own label (slider row, or
    /// toggle/trigger row) — same `slider_ids`/`toggle_ids` lookup
    /// `label_hit` uses. `None` when the param isn't built (hidden param, a
    /// folded D5 section, or an unknown id). Named addressing (not index),
    /// matching `mapping_chevron_rect`'s convention — for a bounds-overlap
    /// assertion over the REAL painted row, e.g. BUG-108's "+ Add Effect
    /// never overlaps the last card row" class-kill.
    pub fn param_row_rect(&self, tree: &UITree, param_id: &str) -> Option<Rect> {
        let i = self.param_info.iter().position(|p| p.param_id == param_id)?;
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
            })?;
        Some(tree.get_bounds(label_id))
    }

    /// The D5 section headers built this frame, as `(node_id, section_name)`
    /// — same data `handle_click` resolves a header click against. Read-only
    /// accessor for cross-module bounds-overlap assertions (e.g. BUG-108's
    /// class-kill in `panels::inspector`'s tests, which can't reach the
    /// private `section_header_ids` field directly).
    pub fn section_header_ids(&self) -> &[(NodeId, String)] {
        &self.section_header_ids
    }

    // ── compute_height ────────────────────────────────────────────

    pub fn compute_height(&self) -> f32 {
        match self.kind {
            ParamCardKind::Effect => self.compute_height_effect(),
            ParamCardKind::Generator => self.compute_height_generator(),
        }
    }

    fn compute_height_effect(&self) -> f32 {
        let h = BORDER_W * 2.0 + HEADER_HEIGHT + self.effect_body_natural_height() * self.collapse_frac();
        h + CARD_BOTTOM_MARGIN
    }

    /// The effect card's param-row block height at full expansion (frac = 1),
    /// including each row's own P1 drawer contribution. `compute_height_effect`
    /// scales this by `collapse_frac()`; `build_effect` sizes the animated
    /// clip-reveal region to it while `collapse_anim` is mid-flight.
    ///
    /// BUG-108: walks `section_runs()` — mirroring `build_effect`'s own draw
    /// loop exactly — instead of summing `param_info` linearly. A linear sum
    /// is blind to the D5 section-header bar every section run draws
    /// (`build_section_header`, `ROW_HEIGHT + ROW_SPACING`) and to a folded
    /// section's rows drawing nothing at all; either one made this height
    /// shorter than what `build_effect` actually painted, so the "+ Add
    /// Effect" button anchored below it (`layer_column_height`) landed
    /// mid-card instead of below the last drawn row.
    fn effect_body_natural_height(&self) -> f32 {
        // The relight block below always draws when expanded (P5b), so the
        // body is never truly empty anymore even with zero regular params.
        let mut h = HEADER_BODY_GAP;
        for (start, len, section) in self.section_runs() {
            if let Some(name) = &section {
                // Section header bar — drawn even when every row in the run
                // is folded away.
                h += ROW_HEIGHT + ROW_SPACING;
                let folded = self.section_folded.get(name).copied().unwrap_or(false);
                if folded {
                    continue;
                }
            }
            for i in start..start + len {
                // Hidden params consume zero vertical space.
                if !self.param_info[i].exposed {
                    continue;
                }
                h += ROW_HEIGHT + ROW_SPACING;
                // A plain toggle never gets a drawer (nothing to modulate) — zero
                // lane, zero height, unconditionally. `is_trigger` and ordinary
                // sliders both go through the general `active_mod_tabs`-driven
                // height (`animated_drawer_height` already handles "no active
                // config → 0" on its own; is_trigger only ever has Audio active,
                // per D5b). `is_trigger_gate` is ALSO an `is_toggle` row (D6) but
                // reaches its own `AudioTrigger` tab through the same path.
                if !self.param_info[i].is_toggle || self.param_info[i].is_trigger_gate {
                    h += self.animated_drawer_height(i);
                }
            }
        }
        h + self.relight_block_height()
    }

    /// Fixed height of the always-visible D3/D4 "3D Shading" block: a
    /// section label + the six knob rows + the Height From row. Drawn
    /// (greyed when off, never hidden — no-conditionally-visible-ui)
    /// regardless of whether the card has any regular params
    /// (`docs/DEPTH_RELIGHT_DESIGN.md` P5b).
    fn relight_block_height(&self) -> f32 {
        // Feature disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // the "3D Shading" block is not drawn, so it contributes no height.
        if !RELIGHT_FEATURE_ENABLED {
            return 0.0;
        }
        const RELIGHT_ROW_COUNT: f32 = 1.0 + 6.0 + 1.0; // label + 6 knobs + Height From
        RELIGHT_ROW_COUNT * (ROW_HEIGHT + ROW_SPACING)
    }

    /// BUG-108: same section-run walk as `effect_body_natural_height` — see
    /// its doc comment. `build_generator` draws section headers identically
    /// to `build_effect` (same `build_section_header` call, same fold-skip),
    /// so this needs the same fix.
    fn compute_height_generator(&self) -> f32 {
        let mut h = BORDER_W * 2.0 + HEADER_HEIGHT;
        if !self.is_collapsed {
            // Always true now — the relight block below always draws when
            // expanded (P5b), so the body is never truly empty.
            h += HEADER_BODY_GAP;
            for (start, len, section) in self.section_runs() {
                if let Some(name) = &section {
                    h += ROW_HEIGHT + ROW_SPACING;
                    let folded = self.section_folded.get(name).copied().unwrap_or(false);
                    if folded {
                        continue;
                    }
                }
                for i in start..start + len {
                    h += ROW_HEIGHT + ROW_SPACING;
                    // Same rule as `effect_body_natural_height`: only a plain
                    // toggle forces zero drawer height. `is_trigger` reaches the
                    // audio-mod drawer (D5b), `is_trigger_gate` reaches the
                    // audio-TRIGGER-mod drawer (D6) — both via the same general
                    // height path every slider row uses.
                    if !self.param_info[i].is_toggle || self.param_info[i].is_trigger_gate {
                        h += self.animated_drawer_height(i);
                    }
                }
            }
            // String param rows (text fields)
            for _ in &self.string_param_info {
                h += ROW_HEIGHT + ROW_SPACING;
            }
            h += self.relight_block_height();
            h += PADDING;
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
        let h = match active.len() {
            0 => return 0.0,
            1 => mod_config_height(active[0], info, &self.state.mod_state, i),
            _ => {
                let stored = self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver);
                let shown = resolve_active_tab(&active, stored).unwrap_or(active[0]);
                MOD_TAB_STRIP_H + mod_config_height(shown, info, &self.state.mod_state, i)
            }
        };
        // Match the build's post-drawer break (see `build_param_row`).
        h + DRAWER_BOTTOM_GAP
    }

    /// Reserved drawer height for row `i`, following the P1 open/close tween while
    /// one is in flight. Equals `row_drawer_height(i)` once settled (and always,
    /// for a card the inspector doesn't tick), so `compute_height` and the build
    /// agree and a settled card lays out exactly as before the motion work.
    fn animated_drawer_height(&self, i: usize) -> f32 {
        match self.drawer_height_anim.get(i) {
            // Only override while a tween is actually in flight. Once settled,
            // `row_drawer_height` is the live source of truth — so a state change
            // that doesn't route through `configure` (e.g. a direct test mutation,
            // or any future in-place edit) is reflected immediately, and build
            // (which also only supplies a reveal while `is_animating`) stays in
            // exact agreement with this reserved height.
            Some(a) if a.is_animating() => a.value(),
            _ => self.row_drawer_height(i),
        }
    }

    /// Force every per-card tween (drawer height, tab-ink slide, collapse,
    /// spawn pop, delete fade, value flash, value snap-back) to its settled
    /// end state in one call (BUG-073 fix shape (b)): a headless `--script`
    /// driver has no per-frame timer, so a tween armed mid-script — e.g. a
    /// newly-armed drawer growing a card's row count — would otherwise never
    /// advance past its t=0 state unless the script happens to insert a
    /// `Step` afterward. Reuses `tick_drawers`/`tick_value_flash`'s own tick
    /// logic with a `dt_ms` large enough that every tween's `t` clamps to 1.0
    /// in one call, rather than duplicating the settle math per-field.
    /// Returns whether anything was actually mid-flight — the caller only
    /// needs to force a rebuild when this is `true`.
    pub fn skip_to_settled(&mut self, tree: &mut UITree) -> bool {
        let was_animating = self.collapse_anim.is_animating()
            || self.spawn_scale.is_animating()
            || self.delete_fade.as_ref().is_some_and(|f| f.progress().is_some())
            || self.drawer_height_anim.iter().any(|a| a.is_animating())
            || self.mod_tab_ink.iter().any(|a| a.is_animating())
            || self.value_flash.iter().any(|f| f.progress().is_some())
            || self.value_snapback.iter().any(|a| a.is_animating());
        if was_animating {
            const HUGE_DT_MS: f32 = 1.0e9;
            self.tick_drawers(HUGE_DT_MS);
            self.tick_value_flash(tree, HUGE_DT_MS);
        }
        was_animating
    }

    /// Advance this card's drawer-height tweens by `dt_ms`; returns true while any
    /// is still in flight. Called by the inspector's per-frame `update()`; the
    /// value it advances is read by the *next* `build()` (which the app's
    /// `drawer_anim_active` poll forces while this returns true).
    pub fn tick_drawers(&mut self, dt_ms: f32) -> bool {
        let mut any = false;
        // P2 card collapse + spawn pop ride the same per-frame rail.
        any |= self.collapse_anim.tick(dt_ms);
        any |= self.spawn_scale.tick(dt_ms);
        if let Some(fade) = self.delete_fade.as_mut() {
            any |= fade.tick(dt_ms);
        }
        for a in &mut self.drawer_height_anim {
            any |= a.tick(dt_ms);
        }
        // D1 tab-ink slide rides the same per-frame rail — one bool bubble-up,
        // no second app-side poll.
        for a in &mut self.mod_tab_ink {
            any |= a.tick(dt_ms);
        }
        any
    }

    /// P2 value-change flash + value snap-back: advance every param's
    /// one-shot `Transient` and paint the value-text color accordingly — an
    /// accent while `progress()` is `Some`, reverted to the normal slider
    /// text color the instant it finishes. A plain style write to an
    /// already-built node (no layout change), so unlike `tick_drawers` this
    /// never needs the app's forced-rebuild poll; it just needs to run every
    /// frame, which it already does from the same
    /// `InspectorCompositePanel::update()` call site.
    ///
    /// Also drives P2 "value snap-back" (D15, `value_snapback`/
    /// `begin_value_snapback`): `sync_values`'s dirty-check only calls
    /// `BitmapSlider::update_value` the ONE frame the model value actually
    /// changes, so a settling fill needs its own per-frame repaint here for
    /// every frame after that — `tick`ing `value_snapback[i]` and
    /// re-positioning just the fill/thumb (never the text, which is already
    /// correct — the data snapped instantly) at the eased normalized value.
    pub fn tick_value_flash(&mut self, tree: &mut UITree, dt_ms: f32) -> bool {
        let mut any = false;
        for (i, flash) in self.value_flash.iter_mut().enumerate() {
            let was_active = flash.progress().is_some();
            let still_active = flash.tick(dt_ms);
            any |= still_active;
            if !still_active && !was_active {
                continue; // idle both before and after — nothing to repaint
            }
            let Some(ref ids) = self.slider_ids[i] else {
                continue;
            };
            // Read-modify-write on the node's existing style so bg/radius/font
            // (which differ between the effect and generator sliders) are
            // never guessed at here — only `text_color` changes.
            let Some(mut style) = tree.get_node(ids.value_text).map(|n| n.style) else {
                continue;
            };
            style.text_color = if still_active {
                color::ACCENT_BLUE_C32
            } else {
                // Just finished this tick — revert once, not every frame after.
                color::SLIDER_TEXT_C32
            };
            tree.set_style(ids.value_text, style);
        }
        for (i, anim) in self.value_snapback.iter_mut().enumerate() {
            if !anim.is_animating() {
                continue;
            }
            any |= anim.tick(dt_ms);
            let Some(ref ids) = self.slider_ids[i] else {
                continue;
            };
            // Re-derive the (already-settled) value text rather than touch
            // it — only the fill/thumb position eases; `update_value` writes
            // all three, so pass the unchanged text back through unmodified.
            let Some(text) = tree.get_node(ids.value_text).map(|n| n.text.clone().unwrap_or_default())
            else {
                continue;
            };
            BitmapSlider::update_value(tree, ids, anim.value(), &text);
        }
        any
    }

    /// D1 "tab-ink slide": after a row's mod-config tab strip is built, point
    /// this param's ink tween at the shown tab's on-screen x and draw the
    /// sliding underline. A no-op when fewer than two configs are active (no
    /// strip was built — `self.mod_tab_ids[i]` is empty).
    fn sync_mod_tab_ink(&mut self, tree: &mut UITree, i: usize) {
        let tabs = self.mod_tab_ids[i].clone();
        if tabs.len() < 2 {
            if let Some(ink) = self.mod_tab_ink.get_mut(i) {
                ink.snap(0.0);
            }
            return;
        }
        let shown = resolve_active_tab(
            &tabs.iter().map(|(_, t)| *t).collect::<Vec<_>>(),
            self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
        );
        let Some((id, tab)) = shown.and_then(|s| tabs.iter().find(|(_, t)| *t == s).copied())
        else {
            return;
        };
        let rect = tree.get_bounds(id);
        let Some(ink) = self.mod_tab_ink.get_mut(i) else {
            return;
        };
        if ink.target() == 0.0 && ink.value() == 0.0 {
            ink.snap(rect.x);
        } else {
            ink.set_target(rect.x);
        }
        let ink_y = rect.y + rect.height - MOD_TAB_INK_H;
        tree.add_panel(
            Some(id),
            ink.value(),
            ink_y,
            rect.width,
            MOD_TAB_INK_H,
            UIStyle {
                bg_color: mod_tab_accent(tab),
                ..UIStyle::default()
            },
        );
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
                    .font(HEADER_FONT_SIZE)
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
            // "3D Shading" toggle (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5b) and its
            // leading gap — hidden entirely while the feature is disabled app-wide
            // (`manifold_foundation::RELIGHT_FEATURE_ENABLED`). `Option<View>` is
            // `IntoIterator`, so `children` adds 0 or 1 views.
            .children(RELIGHT_FEATURE_ENABLED.then(gap))
            .children(RELIGHT_FEATURE_ENABLED.then(|| {
                View::button("3D")
                    .w(Sizing::Fixed(RELIGHT_W))
                    .fill_h()
                    .style(toggle_btn_style(self.relight.enabled))
                    .inert()
                    .key(KEY_RELIGHT)
            }))
            .child(gap())
            .child(cog)
            .child(
                // P2 "caret rotate": same single-glyph + rotation technique as
                // the effect header's chevron (`chevron_angle`'s doc comment) —
                // generator cards' `collapse_anim` always snaps rather than
                // eases, so this reads as an instant flip here, same as before.
                View::button("\u{25BC}")
                    .w(Sizing::Fixed(CHEVRON_W))
                    .fill_h()
                    .style(UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Center,
                        transform: Some(Affine2::rotate(self.chevron_angle())),
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
                            .font(HEADER_FONT_SIZE)
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
            )
            // "3D Shading" toggle (`docs/DEPTH_RELIGHT_DESIGN.md` D2/P5b) — hidden
            // entirely while the feature is disabled app-wide
            // (`manifold_foundation::RELIGHT_FEATURE_ENABLED`).
            .children(RELIGHT_FEATURE_ENABLED.then(|| {
                View::button("3D")
                    .fixed(RELIGHT_W, 16.0)
                    .style(toggle_btn_style(self.relight.enabled))
                    .inert()
                    .key(KEY_RELIGHT)
            }));
        // Cog (or a reserved slot in Author) sits LEFT of the chevron so the
        // expand chevron is always the rightmost control — same trailing order as
        // the generator header (… · cog · ▾).
        // P2 "caret rotate": one down-pointing glyph (▼), rotated to ▶ via
        // `chevron_angle()`/`UIStyle.transform` instead of swapping glyphs —
        // see `chevron_angle`'s doc comment.
        let chevron = View::button("\u{25BC}")
            .fixed(CHEVRON_W, 16.0)
            .style(UIStyle {
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                transform: Some(Affine2::rotate(self.chevron_angle())),
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
        // Stacking/hit-test position stays at the UNSCALED rect — only the
        // drawn geometry below pops; a card mid-pop must not jitter its
        // neighbors' reflow or its own drag-reorder hit test.
        self.card_y = rect.y;
        self.param_cache.iter_mut().for_each(|v| *v = f32::NAN);
        self.label_cache.iter_mut().for_each(|v| *v = None);

        // Card frame (border + inner bg) on the host — interactive so clicks on
        // the edge / body select the card (resolved by id in `handle_click`).
        let border_color = self.base_border_color();
        let view = self.effect_frame_view(border_color);
        let h = self.compute_height() - CARD_BOTTOM_MARGIN;
        // D17 "spawn pop": scale the whole card about its own center (the
        // incoming `rect`'s own width/computed height — NOT `rect.height`,
        // which callers pass as a loose bounding box, e.g. tests build at a
        // fixed 300px regardless of the card's real height). Every child
        // node below is positioned from `inner` (`tree.get_bounds` on this
        // scaled frame), so the header/badges/rows pop as one rigid piece
        // with no separate per-child transform — see `spawn_scale`'s doc
        // comment.
        let frame_rect = scaled_card_rect(rect.x, rect.y, rect.width, h, self.spawn_scale.value());
        self.host.build(tree, &view, frame_rect);
        self.first_node = self.host.first_node();
        self.border_id = self.host.node_id_for_key(KEY_BORDER);
        self.inner_bg_id = self.host.node_id_for_key(KEY_INNER);
        self.header_bg_id = self.host.node_id_for_key(KEY_HEADER_BG);
        let inner = tree.get_bounds(self.inner_bg_id.expect("frame built inner bg"));
        let inner_w = inner.width;
        let parent = self.inner_bg_id.expect("frame built inner bg");

        // Header contents (badges + decorations into the host-owned header).
        self.build_effect_header(tree, inner.x, inner.y, inner_w);

        // Param sliders — P2 "card collapse": `collapse_frac()` scales the
        // body's reserved height (see `compute_height_effect`); while
        // `collapse_anim` is mid-flight, the row block builds under a
        // `ClipRegion` sized to the CURRENT animated height (the same
        // top-down-reveal technique `build_param_row`'s per-row P1 drawer
        // tween uses — `param_slider_shared.rs`'s `drawer_parent`) so rows
        // never visually overflow the shrinking/growing card frame. A
        // settled card keeps the exact old behavior: skip entirely when
        // collapsed, build unclipped under `parent` when expanded.
        let frac = self.collapse_frac();
        // The relight rows below draw whenever expanded, regardless of
        // whether this card has any regular params — see the matching
        // comment in `build_generator`.
        if frac > 0.0 {
            let body_y = inner.y + HEADER_HEIGHT + HEADER_BODY_GAP;
            let sliders_parent = if self.collapse_anim.is_animating() {
                tree.add_node(
                    Some(parent),
                    Rect::new(inner.x, body_y, inner_w.max(1.0), (self.effect_body_natural_height() * frac).max(0.0)),
                    UINodeType::ClipRegion,
                    UIStyle::default(),
                    None,
                    UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
                )
            } else {
                parent
            };
            self.build_effect_sliders(tree, sliders_parent, inner.x, body_y, inner_w);
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
        self.relight_btn_id = self.host.node_id_for_key(KEY_RELIGHT);
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

    /// Contiguous runs of `param_info[..].section` — the D5 display-grouping
    /// unit (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): a run is a maximal
    /// span of consecutive rows sharing the same section value (`None`
    /// included — an unsectioned run renders with no header at all). A
    /// repeated section name after a gap is intentionally a SECOND run/header
    /// (display grouping only groups contiguous rows; forbidden move: do not
    /// reorder rows to force contiguity). Returns `(start_index, len,
    /// section)` triples covering `0..param_info.len()` with no gaps.
    fn section_runs(&self) -> Vec<(usize, usize, Option<String>)> {
        let mut runs = Vec::new();
        let mut i = 0;
        while i < self.param_info.len() {
            let section = self.param_info[i].section.clone();
            let mut j = i + 1;
            while j < self.param_info.len() && self.param_info[j].section == section {
                j += 1;
            }
            runs.push((i, j - i, section));
            i = j;
        }
        runs
    }

    /// Build one D5 section-header row: a clickable bar with a fold triangle,
    /// the section name (its own label node, so a UI-flow assertion can match
    /// the bare name exactly), and — when folded — a row-count chip. Returns
    /// the row's own clickable node id; the caller registers `(id, name)`
    /// into `section_header_ids` so `handle_click` can resolve a click back
    /// to the section without a second lookup. Fold state itself lives in
    /// `section_folded` (UI-local workspace state, not serialized — see its
    /// doc comment); this fn only reads `folded`, it does not toggle it.
    fn build_section_header(
        &mut self,
        tree: &mut UITree,
        parent: Option<NodeId>,
        x: f32,
        y: f32,
        w: f32,
        name: &str,
        folded: bool,
        row_count: usize,
        key_base: u64,
    ) -> NodeId {
        let header_id = tree.add_button_keyed(
            parent,
            x,
            y,
            w,
            ROW_HEIGHT,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
            "",
            key_base | ROW_ROLE_SECTION_HEADER,
        );
        let triangle_w = 16.0;
        let triangle = if folded { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
        tree.add_label(
            Some(header_id),
            x + GAP,
            y,
            triangle_w,
            ROW_HEIGHT,
            triangle,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        let count_w = if folded { 40.0 } else { 0.0 };
        tree.add_label(
            Some(header_id),
            x + GAP + triangle_w,
            y,
            (w - GAP * 2.0 - triangle_w - count_w).max(0.0),
            ROW_HEIGHT,
            name,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        if folded {
            tree.add_label(
                Some(header_id),
                x + w - GAP - count_w,
                y,
                count_w,
                ROW_HEIGHT,
                &format!("({row_count})"),
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: color::FONT_CAPTION,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            );
        }
        header_id
    }

    /// Reset every per-index interactive-id slot for row `i` to "nothing
    /// built this frame" — called for a folded section's rows so a stale id
    /// from a PRIOR build (a different frame, a different `UITree`
    /// instance — two trees can mint numerically colliding ids) is never
    /// mistaken for a live widget in the CURRENT tree. Scoped to the D5
    /// fold-skip path only (the pre-existing `!exposed` skip is untouched —
    /// out of scope for this phase).
    fn clear_row_ids(&mut self, i: usize) {
        self.slider_ids[i] = None;
        self.slider_resets[i] = None;
        self.row_catcher_ids[i] = None;
        self.driver_btn_ids[i] = None;
        self.envelope_btn_ids[i] = None;
        self.driver_config_ids[i] = None;
        self.audio_btn_ids[i] = None;
        self.audio_configs[i] = None;
        self.audio_trigger_mode_badge_ids[i] = None;
        self.target_ids[i] = None;
        self.envelope_config_ids[i] = None;
        self.trim_ids[i] = None;
        self.ableton_trim_ids[i] = None;
        self.audio_trim_ids[i] = None;
        self.ableton_config_ids[i] = None;
        self.mapping_chevron_ids[i] = None;
        self.toggle_ids[i] = None;
        self.mod_tab_ids[i] = Vec::new();
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
        // `author` gates both the chevron lane reservation and the glyph
        // draw + row-id scheme below — the lane only exists where the glyph
        // can appear (Author + mappable).
        let author = self.context == CardContext::Author;
        // Label column grows with the row so a wide inspector card gives the
        // param name more room (not just a longer track). Floored at the
        // default, so narrow timeline cards keep the timeline's width exactly.
        // Shared with `build_generator` via `row_geometry` (D2) so the two
        // builders' lane math can't drift from each other.
        let RowGeometry { label_width, slider_w } = row_geometry(w - PADDING * 2.0, author);

        self.section_header_ids.clear();
        let runs = self.section_runs();
        for (start, len, section) in runs {
            if let Some(name) = &section {
                let folded = self.section_folded.get(name).copied().unwrap_or(false);
                let header_id = self.build_section_header(
                    tree,
                    Some(parent),
                    x + PADDING,
                    cy,
                    w - PADDING * 2.0,
                    name,
                    folded,
                    len,
                    (start as u64) << 8,
                );
                self.section_header_ids.push((header_id, name.clone()));
                cy += ROW_HEIGHT + ROW_SPACING;
                if folded {
                    // Folded run: no rows built for start..start+len. Clear
                    // every per-index id explicitly (`clear_row_ids`) rather
                    // than leaving stale ones from a prior build — a fold
                    // toggles at runtime (unlike `!exposed`, an authoring-time
                    // state), so a click on the now-hidden space must never
                    // resolve against a widget from a different frame's tree.
                    for i in start..start + len {
                        self.clear_row_ids(i);
                    }
                    continue;
                }
            }

        for i in start..start + len {
            // Hidden params: leave slider_ids[i] = None and skip widget
            // construction entirely. Slot-index semantics for any attached
            // driver/Ableton mapping/envelope are preserved.
            if !self.param_info[i].exposed {
                continue;
            }
            let info = self.param_info[i].clone();

            if info.is_toggle || info.is_trigger {
                // Toggle / Trigger row — shared builder (Task A of §8.4 P3b:
                // effect cards previously had no branch for this at all and
                // fell through to `build_param_row`, rendering a boolean/
                // fire-once param as a raw draggable slider). Same shared
                // core the generator card uses; effects gate the driver-
                // column reservation on `supports_envelopes` like their
                // slider rows do, so an `is_trigger` row's lone "A" button
                // still lands in the same column.
                let has_osc = self.osc_addresses.get(i).and_then(|a| a.as_ref()).is_some();
                let row = build_toggle_trigger_row(
                    tree,
                    Some(parent),
                    x + PADDING,
                    cy,
                    slider_w,
                    &info,
                    &self.state.mod_state,
                    i,
                    self.param_target(),
                    CONFIG_BTN_FONT_SIZE,
                    self.supports_envelopes,
                    has_osc,
                    author.then_some((i as u64) << 8),
                    // P1 drawer tween: supply the interpolated height only while in
                    // flight; settled rows pass None → the natural (unclipped) layout.
                    self.drawer_height_anim
                        .get(i)
                        .filter(|a| a.is_animating())
                        .map(|a| a.value()),
                );
                self.toggle_ids[i] = Some(ToggleParamIds {
                    label_id: row.label_id,
                    button_id: row.button_id,
                });
                self.toggle_cache[i] = info.default > 0.5;
                self.audio_btn_ids[i] = row.audio_btn;
                self.audio_configs[i] = row.audio_config;
                self.audio_trigger_mode_badge_ids[i] = row.mode_badge_id;
                cy = row.new_cy;
                continue;
            }

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
                &info,
                &self.state.mod_state,
                i,
                self.param_target(),
                &SliderColors::default_slider(),
                CONFIG_BTN_FONT_SIZE,
                self.supports_envelopes,
                label_width,
                self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
                !self.compact,
                author.then_some((i as u64) << 8),
                // P1 drawer tween: supply the interpolated height only while in
                // flight; settled rows pass None → the natural (unclipped) layout.
                self.drawer_height_anim
                    .get(i)
                    .filter(|a| a.is_animating())
                    .map(|a| a.value()),
            );
            self.slider_ids[i] = row.slider;
            self.slider_resets[i] = Some(row.slider_reset);
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
            self.sync_mod_tab_ink(tree, i);
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
                // Naming pass (UI_AUTOMATION_DESIGN.md D8/§3): one static name for
                // every row's chevron — which row comes from the selector's
                // `under_text` query, not a per-row name string.
                if let Some(id) = self.mapping_chevron_ids[i] {
                    tree.set_name(id, "inspector.param_card.mapping_chevron");
                }
            }
            cy = row.new_cy;
        }
        }

        // ── "3D Shading" relight rows (docs/DEPTH_RELIGHT_DESIGN.md P5b) —
        // always drawn, greyed when the header toggle is off (no-
        // conditionally-visible-ui). ──
        self.build_relight_rows(tree, Some(parent), x + PADDING, cy, w - PADDING * 2.0);
    }

    /// The six D3 knob rows + the D4 Height From row — shared between the
    /// effect and generator card, since both hosts read/write the same
    /// `PresetInstance.relight_params` shape (`docs/DEPTH_RELIGHT_DESIGN.md`
    /// P5b). Always drawn when the caller reaches this point (never
    /// conditioned on `self.relight.enabled` — greyed instead, per the
    /// no-conditionally-visible-ui rule), so values set while the toggle is
    /// off survive to when it's switched on.
    fn build_relight_rows(
        &mut self,
        tree: &mut UITree,
        parent: Option<NodeId>,
        x: f32,
        mut cy: f32,
        content_w: f32,
    ) {
        // Feature disabled app-wide (`manifold_foundation::RELIGHT_FEATURE_ENABLED`):
        // draw no "3D Shading" label, knobs, or Height From row. The slider-id
        // slots stay `None`, so drag/reset hit-tests never match.
        if !RELIGHT_FEATURE_ENABLED {
            return;
        }
        let enabled = self.relight.enabled;
        let label_color = if enabled { color::TEXT_PRIMARY_C32 } else { color::TEXT_DIMMED_C32 };
        tree.add_label(
            parent,
            x,
            cy,
            content_w,
            ROW_HEIGHT,
            "3D Shading",
            UIStyle {
                text_color: label_color,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += ROW_HEIGHT + ROW_SPACING;

        let target = self.param_target();
        let colors = if enabled { SliderColors::default_slider() } else { relight_disabled_slider_colors() };
        let label_width = crate::slider::label_width_for_row(content_w);

        for (i, spec) in RELIGHT_FIELD_SPECS.iter().enumerate() {
            let value = self.relight.value(spec.field);
            let norm = BitmapSlider::value_to_normalized(value, spec.min, spec.max);
            let default_norm = BitmapSlider::value_to_normalized(spec.default, spec.min, spec.max);
            let value_text = format!("{value:.2}");
            let reset = PanelAction::slider_reset(
                PanelAction::RelightParamSnapshot(target, spec.field),
                PanelAction::RelightParamChanged(target, spec.field, spec.default),
                PanelAction::RelightParamCommit(target, spec.field),
            );
            let slider = BitmapSlider::build(
                tree,
                parent,
                Rect::new(x, cy, content_w, ROW_HEIGHT),
                Some(spec.label),
                norm,
                &value_text,
                &colors,
                FONT_SIZE,
                label_width,
                default_norm,
                reset,
            );
            self.relight_slider_ids[i] = Some(slider.ids);
            self.relight_slider_resets[i] = Some(slider.reset);
            cy += ROW_HEIGHT + ROW_SPACING;
        }

        // D4 Height From row — a 3-way segmented control.
        let opts = [
            (UiRelightHeightFrom::Auto, "Auto"),
            (UiRelightHeightFrom::Luminance, "Luminance"),
            (UiRelightHeightFrom::InvertedLuminance, "Inverted"),
        ];
        let seg_gap = 2.0;
        let seg_w = (content_w - seg_gap * (opts.len() - 1) as f32) / opts.len() as f32;
        for (i, (opt, text)) in opts.into_iter().enumerate() {
            let active = self.relight.height_from == opt;
            let bg = if enabled && active { color::SLIDER_FILL_C32 } else { color::BG_3 };
            let btn_id = tree.add_button(
                parent,
                x + (seg_w + seg_gap) * i as f32,
                cy,
                seg_w,
                ROW_HEIGHT,
                UIStyle {
                    bg_color: bg,
                    text_color: if enabled { color::TEXT_WHITE_C32 } else { color::TEXT_DIMMED_C32 },
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Center,
                    corner_radius: color::SMALL_RADIUS,
                    ..UIStyle::default()
                },
                text,
            );
            self.relight_height_btn_ids[i] = Some(btn_id);
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
        // D17 "spawn pop" — see the matching comment in `build_effect`. Not
        // currently fired for the generator card (no `reconcile_cards`-style
        // reuse/new detection wired for it — see `spawn_scale`'s call site),
        // but the geometry mechanics are shared so it's a one-line wire-up
        // later if that seam gets added.
        let frame_rect = scaled_card_rect(rect.x, rect.y, rect.width, h, self.spawn_scale.value());
        self.host.build(tree, &view, frame_rect);
        self.first_node = self.host.first_node();
        self.border_id = self.host.node_id_for_key(KEY_BORDER);
        self.inner_bg_id = self.host.node_id_for_key(KEY_INNER);
        self.header_bg_id = self.host.node_id_for_key(KEY_HEADER_BG);
        self.name_label_id = self.host.node_id_for_key(KEY_NAME);
        self.change_btn_id = self.host.node_id_for_key(KEY_CHANGE);
        self.relight_btn_id = self.host.node_id_for_key(KEY_RELIGHT);
        self.chevron_btn_id = self.host.node_id_for_key(KEY_CHEVRON);
        self.cog_btn_id = self.host.node_id_for_key(KEY_COG);
        if let Some(cog) = self.cog_btn_id {
            self.add_cog_dots(tree, cog);
        }

        let inner_x = rect.x + BORDER_W;
        let inner_y = rect.y + BORDER_W;
        let inner_w = rect.width - BORDER_W * 2.0;

        // ── Params (if not collapsed) — the relight rows below always draw
        // when expanded, regardless of whether this card has any regular
        // params (`docs/DEPTH_RELIGHT_DESIGN.md` P5b: "3D Shading" is a
        // per-instance flag independent of the graph's own param list). ──
        if !self.is_collapsed {
            let content_w = inner_w - PADDING * 2.0;
            let cx = inner_x + PADDING;
            let mut cy = inner_y + HEADER_HEIGHT + HEADER_BODY_GAP;
            // Same `row_geometry` helper the effect card uses (D2), so
            // generator slider rows can't drift from the effect card's lane
            // math. `author` gates both the chevron lane reservation and the
            // glyph draw + row-id scheme below.
            let author = self.context == CardContext::Author;
            let RowGeometry { label_width, slider_w } = row_geometry(content_w, author);

            if !self.param_info.is_empty() {
            self.section_header_ids.clear();
            let runs = self.section_runs();
            for (start, len, section) in runs {
                if let Some(name) = &section {
                    let folded = self.section_folded.get(name).copied().unwrap_or(false);
                    let header_id = self.build_section_header(
                        tree,
                        None,
                        cx,
                        cy,
                        content_w,
                        name,
                        folded,
                        len,
                        (start as u64) << 8,
                    );
                    self.section_header_ids.push((header_id, name.clone()));
                    cy += ROW_HEIGHT + ROW_SPACING;
                    if folded {
                        // See the effect-card twin of this branch for why
                        // this clears rather than leaves stale ids.
                        for i in start..start + len {
                            self.clear_row_ids(i);
                        }
                        continue;
                    }
                }

            for i in start..start + len {
                let info = self.param_info[i].clone();

                if info.is_toggle || info.is_trigger {
                    // Toggle / Trigger row — shared builder (Task A of §8.4
                    // P3b unified this with the effect card's toggle/trigger
                    // rendering; see `build_toggle_trigger_row`'s doc comment).
                    // ON/OFF for sticky toggles, ▶ for momentary fire-once
                    // triggers; `is_trigger` additionally reaches the audio-mod
                    // "A" button + drawer (D5b). Click handler dispatches
                    // differently (toggle vs fire) based on the is_trigger flag.
                    let has_osc = self.osc_addresses.get(i).and_then(|a| a.as_ref()).is_some();
                    let row = build_toggle_trigger_row(
                        tree,
                        None,
                        cx,
                        cy,
                        slider_w,
                        &info,
                        &self.state.mod_state,
                        i,
                        self.param_target(),
                        FONT_SIZE,
                        true, // generators always reserve the driver-column gap
                        has_osc,
                        author.then_some((i as u64) << 8),
                        // P1 drawer tween: supply the interpolated height only while in
                        // flight; settled rows pass None → the natural (unclipped) layout.
                        self.drawer_height_anim
                            .get(i)
                            .filter(|a| a.is_animating())
                            .map(|a| a.value()),
                    );
                    self.toggle_ids[i] = Some(ToggleParamIds {
                        label_id: row.label_id,
                        button_id: row.button_id,
                    });
                    self.toggle_cache[i] = info.default > 0.5;
                    self.audio_btn_ids[i] = row.audio_btn;
                    self.audio_configs[i] = row.audio_config;
                    self.audio_trigger_mode_badge_ids[i] = row.mode_badge_id;
                    cy = row.new_cy;
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
                        &info,
                        &self.state.mod_state,
                        i,
                        self.param_target(),
                        &SliderColors::default_slider(),
                        FONT_SIZE,
                        true,
                        label_width,
                        self.mod_active_tab.get(i).copied().unwrap_or(ModTab::Driver),
                        !self.compact,
                        author.then_some((i as u64) << 8),
                        // P1 drawer tween: interpolated height while in flight only.
                        self.drawer_height_anim
                            .get(i)
                            .filter(|a| a.is_animating())
                            .map(|a| a.value()),
                    );
                    self.slider_ids[i] = row.slider;
                    self.slider_resets[i] = Some(row.slider_reset);
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
                    self.sync_mod_tab_ink(tree, i);
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
            }
            } // end if !self.param_info.is_empty()

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

            // ── "3D Shading" relight rows (docs/DEPTH_RELIGHT_DESIGN.md
            // P5b) — always drawn when expanded, greyed when the header
            // toggle is off (no-conditionally-visible-ui). ──
            self.build_relight_rows(tree, None, cx, cy, content_w);
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
        // Shared lookup (checks both slider AND toggle/trigger row labels —
        // effect cards can copy-flash either kind now, same as generator's).
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| self.find_label_name(label_id))
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

        // Per-param slider/toggle/trigger values + label — shared with
        // `sync_values_generator` (`sync_param_value`).
        for (i, slot) in values.iter().enumerate().take(self.param_info.len()) {
            if let Some(b) = self.base_values.get_mut(i) {
                *b = slot.base;
            }
            self.sync_param_value(tree, i, slot.value);
        }
    }

    /// Per-parameter value/label sync shared by both card kinds. Slider rows
    /// redraw their fill + value text on change; a toggle row flips its
    /// ON/OFF button; a trigger row does nothing (the fire counter isn't
    /// user-visible). Kept as one function so the two kinds can't drift back
    /// apart the way `build_effect_sliders` and `build_generator`'s toggle
    /// rendering did (§8.4 P3b Task A).
    fn sync_param_value(&mut self, tree: &mut UITree, i: usize, val: f32) {
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
            // P2 value-change flash: only for a genuine change (not the
            // post-configure NaN resync) and only while this card's slider
            // isn't being dragged (the drag is its own feedback).
            if !self.param_cache[i].is_nan()
                && !self.drag.is_dragging()
                && let Some(flash) = self.value_flash.get_mut(i)
            {
                flash.fire(color::MOTION_SLOW_MS);
            }
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
                // P2 value snap-back (D15): a reset just retargeted this
                // row's `value_snapback` (`begin_value_snapback`, same
                // frame, before this poll) — draw the fill at its
                // just-`snap()`ped starting point instead of jumping
                // straight to `norm`; `tick_value_flash` eases it forward
                // every frame after. Any other value change (drag commit,
                // automation, undo) has no animating snapback here and
                // draws `norm` exactly as before.
                let display_norm = self
                    .value_snapback
                    .get(i)
                    .filter(|a| a.is_animating())
                    .map(|a| a.value())
                    .unwrap_or(norm);
                BitmapSlider::update_value(tree, ids, display_norm, &text);
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
            if let Some(b) = self.base_values.get_mut(i) {
                *b = slot.base;
            }
            self.sync_param_value(tree, i, slot.value);
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

    /// BUG-250: map a [`RowClick::EnumValueCell`] hit to the shared
    /// cycle-or-dropdown action set (`enum_value_cell_actions`). The cell
    /// node id comes from the row's own slider ids (the dropdown anchors
    /// under it); the current value is the synced base value, matching what
    /// the cell displays.
    fn enum_value_cell_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        clicked: NodeId,
    ) -> Vec<PanelAction> {
        let info = &self.param_info[pi];
        let labels = info.value_labels.clone().unwrap_or_default();
        let cell = self
            .slider_ids
            .get(pi)
            .and_then(|s| s.as_ref())
            .map(|s| s.value_text)
            .unwrap_or(clicked);
        let value = self.base_values.get(pi).copied().unwrap_or(info.default);
        enum_value_cell_actions(target, self.pid_at(pi), &labels, value, info.min, cell)
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

    /// A click on an `is_trigger_gate` row's Mode row (§9 U3) — converts the
    /// clicked button index to a `TriggerFireMode` at this dispatch boundary
    /// (this crate mirrors core enums rather than depending on
    /// `manifold-core` directly; see `ui_translate.rs`) and issues one
    /// `AudioModSetTriggerMode`, the same command family every other
    /// audio-mod drawer edit uses.
    fn audio_set_trigger_mode_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        mode_idx: usize,
    ) -> Vec<PanelAction> {
        vec![PanelAction::AudioModSetTriggerMode(target, self.pid_at(pi), mode_idx)]
    }

    pub fn handle_click(&mut self, node_id: NodeId) -> Vec<PanelAction> {
        // "3D Shading" header toggle + D4 Height From row — identical on
        // both card kinds (`docs/DEPTH_RELIGHT_DESIGN.md` P5b), checked
        // once here rather than duplicated in `handle_click_effect`/
        // `handle_click_generator`.
        if self.relight_btn_id == Some(node_id) {
            return vec![PanelAction::RelightToggle(self.param_target())];
        }
        for (i, btn) in self.relight_height_btn_ids.iter().enumerate() {
            if *btn == Some(node_id) {
                let opt = [
                    UiRelightHeightFrom::Auto,
                    UiRelightHeightFrom::Luminance,
                    UiRelightHeightFrom::InvertedLuminance,
                ][i];
                return vec![PanelAction::RelightHeightFromChanged(self.param_target(), opt)];
            }
        }
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

        // D5 section header (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2) →
        // flip this section's fold state (UI-only; no model mutation) and
        // ask for a rebuild so the folded/unfolded rows repaint.
        if let Some((_, name)) = self.section_header_ids.iter().find(|(hid, _)| *hid == id) {
            let name = name.clone();
            let folded = self.section_folded.entry(name).or_insert(false);
            *folded = !*folded;
            return vec![PanelAction::SectionFoldToggled];
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

        // Toggle / Trigger buttons — same button slot, different semantics.
        // is_trigger fires ParamFire (counter +1); is_toggle fires
        // ParamToggle (0↔1 flip). Mirrors `handle_click_generator`'s toggle
        // loop (§8.4 P3b Task A gave effect cards the same toggle/trigger
        // rows generators already had).
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && t.button_id == id
            {
                let is_trigger = self.param_info.get(pi).map(|i| i.is_trigger).unwrap_or(false);
                let target = GraphParamTarget::Effect(ei);
                let action = if is_trigger {
                    PanelAction::ParamFire(target, self.pid_at(pi))
                } else {
                    PanelAction::ParamToggle(target, self.pid_at(pi))
                };
                return vec![action];
            }
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
            &self.state.mod_state,
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
                RowClick::AudioSelectTriggerMode(pi, m) => {
                    self.audio_set_trigger_mode_action(GraphParamTarget::Effect(ei), pi, m)
                }
                RowClick::AudioSelectAction(pi, k) => {
                    vec![PanelAction::AudioModSetActionKind(GraphParamTarget::Effect(ei), self.pid_at(pi), k)]
                }
                RowClick::AudioSelectWrap(pi, w) => {
                    vec![PanelAction::AudioModSetWrap(GraphParamTarget::Effect(ei), self.pid_at(pi), w)]
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
                RowClick::EnumValueCell(pi) => {
                    self.enum_value_cell_action(GraphParamTarget::Effect(ei), pi, id)
                }
            };
        }

        // Toggle labels → copy OSC address (slider labels handled by the
        // shared matcher above — `match_param_row_click`'s `LabelCopy` only
        // checks `slider_ids`). Mirrors `handle_click_generator`.
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && t.label_id == Some(id)
                && let Some(addr) = self.osc_addresses.get(pi).and_then(|a| a.clone())
            {
                self.copied_flash.trigger(id);
                return vec![PanelAction::CopyOscAddress(addr)];
            }
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

        // D5 section header — same fold-toggle as the effect card.
        if let Some((_, name)) = self.section_header_ids.iter().find(|(hid, _)| *hid == id) {
            let name = name.clone();
            let folded = self.section_folded.entry(name).or_insert(false);
            *folded = !*folded;
            return vec![PanelAction::SectionFoldToggled];
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
        // is_trigger fires ParamFire (counter +1); is_toggle fires
        // ParamToggle (0↔1 flip). Was `GenParamFire`/`GenParamToggle`
        // (`ParamId`-only, generator-implied); unified onto `GraphParamTarget`
        // (§8.4 P3b) once effect cards gained the same toggle/trigger rows.
        for (pi, toggle) in self.toggle_ids.iter().enumerate() {
            if let Some(t) = toggle
                && t.button_id == id
            {
                let is_trigger = self
                    .param_info
                    .get(pi)
                    .map(|i| i.is_trigger)
                    .unwrap_or(false);
                let target = GraphParamTarget::Generator;
                let action = if is_trigger {
                    PanelAction::ParamFire(target, self.pid_at(pi))
                } else {
                    PanelAction::ParamToggle(target, self.pid_at(pi))
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
            &self.state.mod_state,
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
                RowClick::AudioSelectTriggerMode(pi, m) => {
                    self.audio_set_trigger_mode_action(GraphParamTarget::Generator, pi, m)
                }
                RowClick::AudioSelectAction(pi, k) => {
                    vec![PanelAction::AudioModSetActionKind(GraphParamTarget::Generator, self.pid_at(pi), k)]
                }
                RowClick::AudioSelectWrap(pi, w) => {
                    vec![PanelAction::AudioModSetWrap(GraphParamTarget::Generator, self.pid_at(pi), w)]
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
                RowClick::EnumValueCell(pi) => {
                    self.enum_value_cell_action(GraphParamTarget::Generator, pi, id)
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
    /// `tree` is read-only and mandatory (BUG-259): all geometry comes from
    /// live bounds, never the build-time cache — in-place scroll shifts node
    /// y without refreshing panel caches (BUG-257).
    pub fn handle_pointer_down(&mut self, node_id: NodeId, pos: Vec2, tree: &UITree) -> Vec<PanelAction> {
        let target = self.param_target();

        // 1. Envelope target handle (the orange grab bar on the slider track).
        for (pi, etarget) in self.target_ids.iter().enumerate() {
            if let Some(t) = etarget
                && node_id == t.target_bar_id
            {
                self.drag.begin(ParamDragTarget::EnvTarget { index: pi }, pos);
                return vec![PanelAction::TargetSnapshot(target, self.pid_at(pi))];
            }
        }

        // 2. Envelope decay slider (in the drawer).
        for (pi, env_cfg) in self.envelope_config_ids.iter().enumerate() {
            if let Some(c) = env_cfg
                && node_id == c.decay_slider.track
            {
                self.drag.begin(ParamDragTarget::EnvDecay { index: pi }, pos);
                let norm = BitmapSlider::x_to_normalized(
                    TrackSpan::of(tree.get_bounds(c.decay_slider.track)),
                    pos.x,
                );
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
                    let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(sl.track)), pos.x).clamp(0.0, 1.0);
                    let value = audio_shape_value_from_norm(which, norm);
                    self.drag.begin(ParamDragTarget::AudioShape { index: pi, param: which }, pos);
                    let pid = self.pid_at(pi);
                    return vec![
                        PanelAction::AudioModShapeSnapshot(target, pid.clone()),
                        PanelAction::AudioModShapeParamChanged(target, pid, which, value),
                    ];
                }
            }
        }

        // 2c. Step-Amount slider (only present while Action=Step, D8) — its
        // own drag slot since `amount` lives on `TriggerAction::Step`, not
        // `AudioModShape` (`AudioShapeParam` doesn't apply here). It's
        // `DrawerIds.sliders[3]`, one past the three shaping sliders above.
        for (pi, audio_cfg) in self.audio_configs.iter().enumerate() {
            let Some((dids, _)) = audio_cfg else { continue };
            if let Some(sl) = dids.sliders.get(3)
                && node_id == sl.track
            {
                let info = &self.param_info[pi];
                let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(sl.track)), pos.x).clamp(0.0, 1.0);
                let mut value = norm_to_step_amount(norm, info.min, info.max);
                if info.whole_numbers {
                    value = value.round();
                }
                self.drag.begin(ParamDragTarget::StepAmount { index: pi }, pos);
                let pid = self.pid_at(pi);
                return vec![
                    PanelAction::AudioModStepAmountSnapshot(target, pid.clone()),
                    PanelAction::AudioModStepAmountChanged(target, pid, value),
                ];
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
            self.drag.begin(ParamDragTarget::Trim { kind, index: pi, is_min }, pos);
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
                    && self.trim_ids.get(pi).and_then(|t| t.as_ref()).is_some()
                {
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
                    // Live bounds + the shared geometry fn: the zone can never
                    // drift from the drawn bars (BUG-258) or a scroll (BUG-259).
                    let bars = trim_bar_rects(tree.get_bounds(ids.track), tmin, tmax);
                    let min_center = bars.min_bar.x + TRIM_BAR_W * 0.5;
                    let max_center = bars.max_bar.x + TRIM_BAR_W * 0.5;
                    let hit_zone = 8.0; // px proximity zone for trim handles

                    let dist_min = (pos.x - min_center).abs();
                    let dist_max = (pos.x - max_center).abs();

                    if dist_min < hit_zone && dist_min <= dist_max {
                        self.drag.begin(
                            ParamDragTarget::Trim { kind: TrimKind::Driver, index: pi, is_min: true },
                            pos,
                        );
                        return vec![PanelAction::TrimSnapshot(TrimKind::Driver, target, self.pid_at(pi))];
                    }
                    if dist_max < hit_zone {
                        self.drag.begin(
                            ParamDragTarget::Trim { kind: TrimKind::Driver, index: pi, is_min: false },
                            pos,
                        );
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
                    let tgt = self
                        .state
                        .mod_state
                        .target_norm
                        .get(pi)
                        .copied()
                        .unwrap_or(1.0);
                    let bar = target_bar_rect(tree.get_bounds(ids.track), tgt);
                    let target_center = bar.x + TARGET_BAR_W * 0.5;
                    if (pos.x - target_center).abs() < 8.0 {
                        self.drag.begin(ParamDragTarget::EnvTarget { index: pi }, pos);
                        return vec![PanelAction::TargetSnapshot(target, self.pid_at(pi))];
                    }
                }

                // No trim/target handle nearby — normal param slider drag
                self.drag.begin(ParamDragTarget::Param { index: pi }, pos);
                let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
                let info = &self.param_info[pi];
                let val = BitmapSlider::normalized_to_value(norm, info.min, info.max);
                let val = if info.whole_numbers { val.round() } else { val };
                return vec![
                    PanelAction::ParamSnapshot(target, self.pid_at(pi)),
                    PanelAction::ParamChanged(target, self.pid_at(pi), val),
                ];
            }
        }

        // D3 relight-knob tracks (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) — same
        // shape as the normal param-slider hit-test above, minus the
        // trim/target overlay checks (relight rows carry no modulation).
        // Always live even while the toggle is off (rows render greyed, not
        // hidden, and edits while off must still take effect).
        for (slider, spec) in self.relight_slider_ids.iter().zip(RELIGHT_FIELD_SPECS.iter()) {
            if let Some(ids) = slider
                && node_id == ids.track
            {
                let field = spec.field;
                self.drag.begin(ParamDragTarget::Relight { field }, pos);
                let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
                let val = BitmapSlider::normalized_to_value(norm, spec.min, spec.max);
                return vec![
                    PanelAction::RelightParamSnapshot(target, field),
                    PanelAction::RelightParamChanged(target, field, val),
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
        if let Some(pi) = self.drag.env_target_index()
            && let Some(slider) = self.slider_ids.get(pi).and_then(|s| s.as_ref())
        {
            // Live bounds, not the cached `track_rect`: in-place scroll shifts
            // the tree nodes without refreshing the cache, so its y is stale.
            let track_rect = tree.get_bounds(slider.track);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos.x);
            if let Some(v) = self.state.mod_state.target_norm.get_mut(pi) {
                *v = norm;
            }
            if let Some(t) = self.target_ids.get(pi).and_then(|t| t.as_ref()) {
                tree.set_bounds(t.target_bar_id, target_bar_rect(track_rect, norm));
            }
            let pid = self.pid_at(pi);
            return match self.kind {
                ParamCardKind::Effect => vec![PanelAction::TargetChanged(GraphParamTarget::Effect(ei), pid, norm)],
                ParamCardKind::Generator => vec![PanelAction::TargetChanged(GraphParamTarget::Generator, pid, norm)],
            };
        }

        // Envelope decay slider drag — update the drawer slider's fill + value,
        // dispatch the decay change (in beats).
        if let Some(pi) = self.drag.env_decay_index()
            && let Some(cfg) = self.envelope_config_ids.get(pi).and_then(|c| c.as_ref())
        {
            let norm = BitmapSlider::x_to_normalized(
                TrackSpan::of(tree.get_bounds(cfg.decay_slider.track)),
                pos.x,
            )
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

        // Audio shaping slider drag — update fill + value, dispatch live edit.
        if let Some((pi, which)) = self.drag.audio_shape() {
            let si = match which {
                AudioShapeParam::Sensitivity => 0,
                AudioShapeParam::Attack => 1,
                AudioShapeParam::Release => 2,
            };
            let track_id = self
                .audio_configs
                .get(pi)
                .and_then(|c| c.as_ref())
                .and_then(|(d, _)| d.sliders.get(si))
                .map(|sl| sl.track);
            if let Some(track_id) = track_id {
                let norm = BitmapSlider::x_to_normalized(
                    TrackSpan::of(tree.get_bounds(track_id)),
                    pos.x,
                )
                .clamp(0.0, 1.0);
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

        // Step-Amount slider drag (D8) — its own path, see `handle_pointer_down`
        // 2c. Updates fill + value and dispatches the live edit.
        if let Some(pi) = self.drag.step_amount() {
            let track_id = self
                .audio_configs
                .get(pi)
                .and_then(|c| c.as_ref())
                .and_then(|(d, _)| d.sliders.get(3))
                .map(|sl| sl.track);
            if let Some(track_id) = track_id {
                let info = &self.param_info[pi];
                let norm = BitmapSlider::x_to_normalized(
                    TrackSpan::of(tree.get_bounds(track_id)),
                    pos.x,
                )
                .clamp(0.0, 1.0);
                let mut value = norm_to_step_amount(norm, info.min, info.max);
                if info.whole_numbers {
                    value = value.round();
                }
                if let Some(v) = self.state.mod_state.audio_step_amount.get_mut(pi) {
                    *v = value;
                }
                let text =
                    if info.whole_numbers { format!("{value:.0}") } else { format!("{value:.2}") };
                let display_norm = step_amount_to_norm(value, info.min, info.max);
                if let Some((d, _)) = self.audio_configs.get(pi).and_then(|c| c.as_ref())
                    && let Some(sl) = d.sliders.get(3)
                {
                    BitmapSlider::update_value(tree, sl, display_norm, &text);
                }
                let pid = self.pid_at(pi);
                return match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::AudioModStepAmountChanged(GraphParamTarget::Effect(ei), pid, value)]
                    }
                    ParamCardKind::Generator => vec![PanelAction::AudioModStepAmountChanged(
                        GraphParamTarget::Generator,
                        pid,
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
        if let Some((kind, pi, is_min)) = self.drag.trim()
            && let Some(track_id) = self
                .slider_ids
                .get(pi)
                .and_then(|s| s.as_ref())
                .map(|s| s.track)
            && let Some((cur_min, cur_max)) = self.trim_range(kind, pi)
        {
            // Live bounds, not the cached `track_rect`: in-place scroll shifts
            // the tree nodes without refreshing the cache, and feeding its
            // stale y to `reposition_trim_bars` teleports the bars off the
            // slider (BUG-257).
            let track_rect = tree.get_bounds(track_id);
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(track_rect), pos.x);
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
        if let Some(pi) = self.drag.param_index()
            && let Some(ids) = self.slider_ids.get(pi).and_then(|s| s.as_ref())
        {
            let info = &self.param_info[pi];
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
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

        // D3 relight-knob drag (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) — mirrors
        // the plain param-slider drag above exactly, minus the value cache
        // (relight knobs have no per-row `param_info`/`param_cache` slot).
        if let Some(field) = self.drag.relight_field()
            && let Some(i) = RELIGHT_FIELD_SPECS.iter().position(|s| s.field == field)
            && let Some(ids) = self.relight_slider_ids[i].as_ref()
        {
            let spec = &RELIGHT_FIELD_SPECS[i];
            let norm = BitmapSlider::x_to_normalized(TrackSpan::of(tree.get_bounds(ids.track)), pos.x);
            let val = BitmapSlider::normalized_to_value(norm, spec.min, spec.max);
            let display_norm = BitmapSlider::value_to_normalized(val, spec.min, spec.max);
            self.relight.set_value(field, val);
            BitmapSlider::update_value(tree, ids, display_norm, &format!("{val:.2}"));
            return vec![PanelAction::RelightParamChanged(self.param_target(), field, val)];
        }

        Vec::new()
    }

    /// Drag-end dispatch — commit the active drag. Identical bookkeeping for
    /// both kinds; only the emitted [`PanelAction`] variant differs.
    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        let ei = self.effect_index;

        match self.drag.end() {
            Some(ParamDragTarget::EnvTarget { index: pi }) => {
                let pid = self.pid_at(pi);
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::TargetCommit(GraphParamTarget::Effect(ei), pid)],
                    ParamCardKind::Generator => vec![PanelAction::TargetCommit(GraphParamTarget::Generator, pid)],
                }
            }
            Some(ParamDragTarget::EnvDecay { index: pi }) => {
                let pid = self.pid_at(pi);
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::EnvDecayCommit(GraphParamTarget::Effect(ei), pid)],
                    ParamCardKind::Generator => vec![PanelAction::EnvDecayCommit(GraphParamTarget::Generator, pid)],
                }
            }
            Some(ParamDragTarget::AudioShape { index: pi, .. }) => {
                let pid = self.pid_at(pi);
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::AudioModShapeCommit(GraphParamTarget::Effect(ei), pid)],
                    ParamCardKind::Generator => {
                        vec![PanelAction::AudioModShapeCommit(GraphParamTarget::Generator, pid)]
                    }
                }
            }
            Some(ParamDragTarget::StepAmount { index: pi }) => {
                let pid = self.pid_at(pi);
                match self.kind {
                    ParamCardKind::Effect => {
                        vec![PanelAction::AudioModStepAmountCommit(GraphParamTarget::Effect(ei), pid)]
                    }
                    ParamCardKind::Generator => {
                        vec![PanelAction::AudioModStepAmountCommit(GraphParamTarget::Generator, pid)]
                    }
                }
            }
            Some(ParamDragTarget::Trim { kind, index: pi, .. }) => {
                let pid = self.pid_at(pi);
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::TrimCommit(kind, GraphParamTarget::Effect(ei), pid)],
                    ParamCardKind::Generator => {
                        vec![PanelAction::TrimCommit(kind, GraphParamTarget::Generator, pid)]
                    }
                }
            }
            Some(ParamDragTarget::Param { index: pi }) => {
                let pid = self.pid_at(pi);
                match self.kind {
                    ParamCardKind::Effect => vec![PanelAction::ParamCommit(GraphParamTarget::Effect(ei), pid)],
                    ParamCardKind::Generator => vec![PanelAction::ParamCommit(GraphParamTarget::Generator, pid)],
                }
            }
            Some(ParamDragTarget::Relight { field }) => {
                vec![PanelAction::RelightParamCommit(self.param_target(), field)]
            }
            None => Vec::new(),
        }
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

        // Every materialised slider's right-click reset — main rows AND every
        // drawer slider (audio-shape Amount/Attack/Release, envelope Decay) —
        // replayed independent of row kind (slider / toggle / trigger /
        // trigger-gate). This is what fixes BUG-070: a trigger-gate row has no
        // main slider, but its armed drawer's sliders are stored in
        // `audio_configs[pi]` regardless, so this pass reaches them directly
        // instead of piggybacking on the main-slider loop below (which is
        // exactly the loop that used to bail before ever checking
        // `audio_configs`).
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let (Some(ids), Some(reset)) =
                (slider, self.slider_resets.get(pi).and_then(|r| r.as_ref()))
            {
                BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }
        for cfg in self.envelope_config_ids.iter().flatten() {
            BitmapSlider::register_track_reset(&cfg.decay_slider, &cfg.decay_reset, intents);
        }
        for cfg in self.audio_configs.iter().flatten() {
            let (dids, _) = cfg;
            for (sl, reset) in dids.sliders.iter().zip(dids.slider_resets.iter()) {
                BitmapSlider::register_track_reset(sl, reset, intents);
            }
        }
        // D3 relight-knob resets (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) — same
        // pattern as the main-row loop above.
        for (ids, reset) in self.relight_slider_ids.iter().zip(self.relight_slider_resets.iter()) {
            if let (Some(ids), Some(reset)) = (ids, reset) {
                BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }

        // Per-param perform-mapping menu.
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            // Generator toggle/trigger rows have no map gesture — they fall
            // through to the card claim like any other dead zone.
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

            // Rest of the row → perform-mapping menu (Perform context only;
            // Author uses the right-edge mapping drawer instead). Registered on
            // both the interactive label and the full-row catcher behind the
            // value cell + gaps, so a right-click anywhere on the row that isn't
            // the track reliably opens the param menu — no narrow-target lottery.
            if self.context == CardContext::Perform {
                let menu = PanelAction::ParamLabelRightClick(target, self.pid_at(pi));
                // Label registration goes through the contract (P3/D14).
                BitmapSlider::register_label_mapping(ids, &menu, intents);
                // The row catcher is a second node carrying the SAME action
                // — host-attached chrome, not a contract zone (it's a
                // full-row dead-zone catcher behind the value cell + gaps,
                // no `SliderZone` of its own), so it stays hand-registered.
                if let Some(Some(catcher)) = self.row_catcher_ids.get(pi).copied() {
                    intents.claim_area(catcher);
                    intents.on(catcher, RightClick, menu.clone());
                }
                // The value cell carries the same menu: it wins the hit-test
                // over the catcher (BUG-250's fix made it interactive per its
                // zone contract), and `ValueCell + RightClick` is a contract
                // dead stop hosts may bind (D13) — binding it keeps the
                // pre-fix "right-click anywhere off-track opens the menu"
                // behavior instead of degrading to the card menu.
                intents.on(ids.value_text, RightClick, menu);
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
                    is_trigger_gate: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                    section: None,
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
                    is_trigger_gate: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                    section: None,
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
            automation_active: vec![false; n],
            automation_overridden: vec![false; n],
            relight: RelightCardConfig::default(),
        }
    }

    /// Config with a third (`is_toggle`) and fourth (`is_trigger`) param —
    /// exercises the effect card's toggle/trigger row rendering + click
    /// dispatch (§8.4 P3b: effect cards previously had no branch for either
    /// and rendered them as raw sliders — the Task A bug).
    fn effect_config_with_toggle_and_trigger() -> ParamCardConfig {
        let mut c = effect_config();
        c.params.push(ParamInfo {
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
            is_trigger_gate: false,
            value_labels: None,
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        });
        c.params.push(ParamInfo {
            param_id: std::borrow::Cow::Borrowed("reset"),
            name: "Reset".into(),
            min: 0.0,
            max: 0.0,
            default: 0.0,
            whole_numbers: true,
            is_angle: false,
            exposed: true,
            is_toggle: false,
            is_trigger: true,
            is_trigger_gate: false,
            value_labels: None,
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        });
        let n = c.params.len();
        c.driver_active.resize(n, false);
        c.envelope_active.resize(n, false);
        c.trim_min.resize(n, 0.0);
        c.trim_max.resize(n, 1.0);
        c.target_norm.resize(n, 1.0);
        c.env_decay.resize(n, 1.0);
        c.driver_beat_div_idx.resize(n, -1);
        c.driver_waveform_idx.resize(n, -1);
        c.driver_reversed.resize(n, false);
        c.driver_dotted.resize(n, false);
        c.driver_triplet.resize(n, false);
        c.driver_free_period.resize(n, None);
        c.automation_active.resize(n, false);
        c.automation_overridden.resize(n, false);
        c
    }

    #[test]
    fn build_effect_toggle_and_trigger_rows() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        // Task A: a toggle/trigger param must build a toggle row (button),
        // NOT a slider — the bug was `build_effect_sliders` calling
        // `build_param_row` unconditionally for every param.
        assert!(panel.slider_ids[0].is_some()); // Radius = slider
        assert!(panel.slider_ids[1].is_some()); // Strength = slider
        assert!(panel.slider_ids[2].is_none()); // Invert = toggle, no slider
        assert!(panel.slider_ids[3].is_none()); // Reset = trigger, no slider
        assert!(panel.toggle_ids[2].is_some());
        assert!(panel.toggle_ids[3].is_some());

        // Task B (D5b): the trigger row reaches the audio-mod "A" button;
        // the toggle row does not (zero D/E/A lane, unchanged rule).
        assert!(panel.audio_btn_ids[2].is_none());
        assert!(panel.audio_btn_ids[3].is_some());
    }

    #[test]
    fn handle_click_effect_toggle_param() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let button_id = panel.toggle_ids[2].as_ref().unwrap().button_id;
        let actions = panel.handle_click(button_id);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::ParamToggle(target, param_id) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "invert");
            }
            other => panic!("expected ParamToggle, got {:?}", other),
        }
    }

    #[test]
    fn handle_click_effect_trigger_param() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let button_id = panel.toggle_ids[3].as_ref().unwrap().button_id;
        let actions = panel.handle_click(button_id);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::ParamFire(target, param_id) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "reset");
            }
            other => panic!("expected ParamFire, got {:?}", other),
        }

        // The trigger row's "A" button reaches the shared audio-mod dispatch.
        let audio_btn = panel.audio_btn_ids[3].unwrap();
        let actions = panel.handle_click(audio_btn);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::OpenAudioSetup | PanelAction::AudioModToggle(..)));
    }

    /// Config with an `is_trigger_gate` toggle param (§9, the outer-card gate
    /// for a generator's/effect's audio trigger response — Strobe's/the 11
    /// generators' `clip_trigger`), armed with a real `ParameterAudioMod` (a
    /// `trigger_mode`, not a separate config type) so the drawer builds.
    /// Exercises `build_toggle_trigger_row`'s `is_trigger_gate` branch riding
    /// the SAME standard audio-mod drawer `effect_config_with_toggle_and_
    /// trigger`'s `is_trigger` (D5b) coverage above exercises, plus the
    /// trailing Mode row.
    fn effect_config_with_trigger_gate() -> ParamCardConfig {
        let mut c = effect_config();
        c.params.push(ParamInfo {
            param_id: std::borrow::Cow::Borrowed("clip_trigger"),
            name: "Clip Trigger".into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            whole_numbers: false,
            is_angle: false,
            exposed: true,
            is_toggle: true,
            is_trigger: false,
            is_trigger_gate: true,
            value_labels: None,
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        });
        let n = c.params.len();
        c.driver_active.resize(n, false);
        c.envelope_active.resize(n, false);
        c.trim_min.resize(n, 0.0);
        c.trim_max.resize(n, 1.0);
        c.target_norm.resize(n, 1.0);
        c.env_decay.resize(n, 1.0);
        c.driver_beat_div_idx.resize(n, -1);
        c.driver_waveform_idx.resize(n, -1);
        c.driver_reversed.resize(n, false);
        c.driver_dotted.resize(n, false);
        c.driver_triplet.resize(n, false);
        c.driver_free_period.resize(n, None);
        c.automation_active.resize(n, false);
        c.automation_overridden.resize(n, false);

        c.audio.send_labels = vec!["Kick".into()];
        c.audio.send_ids = vec![manifold_foundation::AudioSendId::new("send-kick")];
        c.audio.active = vec![false; n];
        c.audio.send_id = vec![None; n];
        c.audio.kind_idx = vec![0; n];
        c.audio.band_idx = vec![0; n];
        c.audio.range_min = vec![0.0; n];
        c.audio.range_max = vec![1.0; n];
        c.audio.invert = vec![false; n];
        c.audio.rate = vec![false; n];
        c.audio.sensitivity = vec![1.0; n];
        c.audio.attack_ms = vec![5.0; n];
        c.audio.release_ms = vec![120.0; n];
        c.audio.trigger_mode_idx = vec![0; n];
        let gi = n - 1; // the clip_trigger row's index
        c.audio.active[gi] = true;
        c.audio.send_id[gi] = Some(manifold_foundation::AudioSendId::new("send-kick"));
        c.audio.band_idx[gi] = 1; // Low
        c.audio.sensitivity[gi] = 0.65;
        c.audio.trigger_mode_idx[gi] = 2; // Both
        c
    }

    #[test]
    fn build_effect_trigger_gate_row_and_drawer() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_trigger_gate());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let gi = panel.param_info.len() - 1;
        // Renders as a toggle row (not a slider), same as a plain toggle —
        // but ALSO reaches the standard audio-mod "A" button + drawer, which
        // a plain toggle never does.
        assert!(panel.slider_ids[gi].is_none());
        assert!(panel.toggle_ids[gi].is_some());
        assert!(panel.audio_btn_ids[gi].is_some());
        // Armed in the fixture (`active[gi] = true`) — the drawer must build.
        assert!(panel.audio_configs[gi].is_some());
        // The collapsed-row mode badge exists (mode = Both, index 2 > 0).
        assert!(panel.audio_trigger_mode_badge_ids[gi].is_some());
    }

    #[test]
    fn open_fire_mode_drawer_send_and_band_read_the_armed_trigger_gate_row() {
        // P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item
        // 5): the fixture arms the clip_trigger row on send "send-kick",
        // band index 1 (Low) — the accessors must report exactly that.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_trigger_gate());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        assert_eq!(
            panel.open_fire_mode_drawer_send(),
            Some(manifold_foundation::AudioSendId::new("send-kick"))
        );
        assert_eq!(panel.open_fire_mode_drawer_band(), Some(crate::types::AudioBand::Low));
    }

    #[test]
    fn open_fire_mode_drawer_send_is_none_when_disarmed() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        let mut cfg = effect_config_with_trigger_gate();
        let gi = cfg.params.len() - 1;
        cfg.audio.active[gi] = false; // disarmed — drawer never builds
        panel.configure(&cfg);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        assert_eq!(panel.open_fire_mode_drawer_send(), None);
        assert_eq!(panel.open_fire_mode_drawer_band(), None);
    }

    #[test]
    fn open_fire_mode_drawer_send_is_none_for_a_plain_continuous_mod() {
        // Negative gate (§7.3 P7): an armed but NON-trigger-gate audio mod's
        // open drawer must never re-tap the scope — only fire-mode configs do.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        let mut cfg = effect_config_with_trigger_gate();
        let gi = cfg.params.len() - 1;
        // Same armed row, reshaped into a plain continuous (non-toggle,
        // non-trigger) param — a genuine non-gate shape (not just a flag flip
        // on the toggle-row fixture), which still shows an Amount meter but
        // must never re-tap the scope send/band.
        cfg.params[gi].is_trigger_gate = false;
        cfg.params[gi].is_toggle = false;
        panel.configure(&cfg);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        assert!(panel.audio_configs[gi].is_some(), "sanity: the drawer still builds");
        assert_eq!(panel.open_fire_mode_drawer_send(), None);
        assert_eq!(panel.open_fire_mode_drawer_band(), None);
    }

    #[test]
    fn handle_click_effect_trigger_gate_drawer() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_trigger_gate());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
        let gi = panel.param_info.len() - 1;

        // The "A" button toggles the mod (armed → disarm, since the fixture
        // starts active) through the SAME `AudioModToggle` every other
        // audio-mod row uses.
        let audio_btn = panel.audio_btn_ids[gi].unwrap();
        let actions = panel.handle_click(audio_btn);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::AudioModToggle(target, param_id) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "clip_trigger");
            }
            other => panic!("expected AudioModToggle, got {:?}", other),
        }

        // The drawer's Source (send) button — flat index 0 (only one send).
        // Clone the button ids out first: `handle_click` needs `&mut panel`,
        // which would otherwise conflict with the borrow of `dids`.
        let (dids, send_count) = panel.audio_configs[gi].as_ref().unwrap();
        assert_eq!(*send_count, 1);
        let button_ids: Vec<NodeId> = dids.button_ids().to_vec();
        let send_btn = button_ids[0];
        let actions = panel.handle_click(send_btn);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::AudioModSetSource(target, param_id, send_id, _feature) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "clip_trigger");
                assert_eq!(send_id.as_ref(), "send-kick");
            }
            other => panic!("expected AudioModSetSource, got {:?}", other),
        }

        // The Mode row's last button ("Both") — flat index = send_count(1) +
        // kind_count(8) + band_count(4) + 1 (Invert — Delta removed §7.2
        // item 2) + 2 (Both is the Mode row's 3rd button, index 2).
        let mode_both_btn = button_ids[1 + AUDIO_KIND_COUNT + 4 + 1 + 2];
        let actions = panel.handle_click(mode_both_btn);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::AudioModSetTriggerMode(target, param_id, mode_idx) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "clip_trigger");
                assert_eq!(*mode_idx, 2);
            }
            other => panic!("expected AudioModSetTriggerMode, got {:?}", other),
        }
    }

    #[test]
    fn trigger_gate_drawer_tween_clips_midflight() {
        // BUG-060 layer 2: `build_toggle_trigger_row`'s audio-mod drawer had no
        // reveal clip at all (unlike `build_param_row`'s tween path) — a
        // mid-flight open/close on an `is_trigger_gate` row rendered its full
        // natural height every frame instead of growing in. Same end-to-end
        // shape as `drawer_open_tween_reserves_interpolated_height_clips_then_
        // settles` above, but for the toggle/trigger row path specifically.
        let mut closed = effect_config_with_trigger_gate();
        let gi = closed.params.len() - 1;
        closed.audio.active[gi] = false; // start disarmed — drawer closed
        let mut panel = ParamCardPanel::new();
        panel.configure(&closed);
        let closed_h = panel.compute_height();

        // Re-arm: retargets the tween to the full drawer height.
        panel.configure(&effect_config_with_trigger_gate()); // active[gi] = true
        assert!(
            panel.drawer_height_anim[gi].is_animating(),
            "arming the trigger-gate row's audio mod retargets the drawer tween"
        );
        let full_target = panel.row_drawer_height(gi);
        assert!(full_target > 0.0);

        panel.tick_drawers(40.0);
        assert!(panel.drawer_height_anim[gi].is_animating(), "still mid-flight after 40ms");

        let mut tree = UITree::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
        let clips_midflight =
            tree.nodes().iter().filter(|n| n.node_type == UINodeType::ClipRegion).count();
        assert!(clips_midflight >= 1, "an animating trigger-gate drawer builds under a clip region");
        // The drawer still builds (not skipped) — just clipped to the reveal.
        assert!(panel.audio_configs[gi].is_some());

        for _ in 0..20 {
            panel.tick_drawers(20.0);
        }
        assert!(!panel.drawer_height_anim[gi].is_animating(), "tween settles");
        assert!(
            (panel.compute_height() - (closed_h + full_target)).abs() < 0.1,
            "settled height = closed + full drawer contribution"
        );
        let mut tree2 = UITree::new();
        panel.build(&mut tree2, Rect::new(0.0, 0.0, 280.0, 400.0));
        let clips_settled =
            tree2.nodes().iter().filter(|n| n.node_type == UINodeType::ClipRegion).count();
        assert!(
            clips_settled < clips_midflight,
            "settled build drops the trigger-gate drawer clip: settled={clips_settled} midflight={clips_midflight}"
        );
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
        let before = tree.get_node(border).unwrap().style.bg_color;
        panel.update_selection_visual(&mut tree, true);
        assert_eq!(
            tree.get_node(border).unwrap().style.bg_color,
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
    fn value_change_flash_fires_on_genuine_change_not_the_initial_resync() {
        // P2 value-change flash: `configure()` resets `param_cache` to NaN, so
        // the very next `sync_values` is a resync (every param "changed" only
        // because the cache was cleared) — that must NOT flash. A second,
        // genuinely different value must.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        use crate::view::UiParamSlot as ParamSlot;
        panel.sync_values(&mut tree, &[ParamSlot::exposed(50.0), ParamSlot::exposed(0.8)]);
        assert!(
            panel.value_flash[0].progress().is_none(),
            "the post-configure resync must not flash"
        );

        panel.sync_values(&mut tree, &[ParamSlot::exposed(75.0), ParamSlot::exposed(0.8)]);
        assert!(
            panel.value_flash[0].progress().is_some(),
            "a genuine value change fires the flash"
        );
        assert!(panel.value_flash[1].progress().is_none(), "unchanged param 1 stays idle");

        // tick_value_flash paints the accent while active, then reverts once —
        // the id-based read-modify-write leaves everything else on the node
        // untouched (font/bg/align), only `text_color` moves.
        let value_text_id = panel.slider_ids[0].as_ref().unwrap().value_text;
        panel.tick_value_flash(&mut tree, 1.0);
        assert_eq!(tree.get_node(value_text_id).unwrap().style.text_color, color::ACCENT_BLUE_C32);

        for _ in 0..30 {
            panel.tick_value_flash(&mut tree, color::MOTION_SLOW_MS / 20.0);
        }
        assert!(panel.value_flash[0].progress().is_none(), "flash finishes");
        assert_eq!(
            tree.get_node(value_text_id).unwrap().style.text_color,
            color::SLIDER_TEXT_C32,
            "reverted to the normal slider text color once finished"
        );
    }

    #[test]
    fn value_snapback_eases_fill_from_old_to_default_after_reset() {
        // P2 "value snap-back" (D15), end to end: `begin_value_snapback` is
        // called the instant the RIGHT-CLICK reset commits (data already
        // snapped by that point, per the app-side dispatch handler) —
        // `sync_values`'s next poll (simulating the model now reading the
        // new default) must draw the fill at the EASED position, not jump
        // straight to the final one, and it must reach the final width once
        // the tween settles.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        use crate::view::UiParamSlot as ParamSlot;
        // Row 0 is "radius", min 0 / max 100 / default 10.
        panel.sync_values(&mut tree, &[ParamSlot::exposed(50.0), ParamSlot::exposed(0.8)]);
        let fill_id = panel.slider_ids[0].as_ref().unwrap().fill;
        let width_at_50 = tree.get_bounds(fill_id).width;

        // The reset gesture: data goes 50 -> 10 instantly; this only starts
        // the visual ease.
        panel.begin_value_snapback(&std::borrow::Cow::Borrowed("radius"), 50.0, 10.0);
        assert!(panel.value_snapback[0].is_animating(), "reset starts the row's own tween");

        // The next poll sees the model already at the new default (10.0).
        panel.sync_values(&mut tree, &[ParamSlot::exposed(10.0), ParamSlot::exposed(0.8)]);
        let width_just_after = tree.get_bounds(fill_id).width;
        assert!(
            (width_just_after - width_at_50).abs() < 0.5,
            "the fill must NOT jump to the final width the instant the model value \
             changes — it starts from wherever it was: {width_just_after} vs {width_at_50}"
        );

        // Tick forward: `tick_value_flash` (the only per-frame driver of
        // `value_snapback`, since `sync_values` no longer sees a dirty value)
        // must keep repainting the fill every frame until it settles at the
        // final (smaller — 10 < 50) width.
        for _ in 0..30 {
            panel.tick_value_flash(&mut tree, color::MOTION_MED_MS / 20.0);
        }
        assert!(!panel.value_snapback[0].is_animating(), "tween settles");
        let width_settled = tree.get_bounds(fill_id).width;
        assert!(
            width_settled < width_just_after,
            "settled fill reflects the smaller default value: {width_settled} vs {width_just_after}"
        );

        // A reset that's a no-op (already-at-default) `begin_value_snapback`
        // targeting the SAME value must not start an animation.
        panel.begin_value_snapback(&std::borrow::Cow::Borrowed("radius"), 10.0, 10.0);
        assert!(!panel.value_snapback[0].is_animating(), "same-value retarget is a no-op");
    }

    #[test]
    fn value_change_flash_suppressed_while_dragging() {
        // The drag itself is the feedback for the row being dragged — a flash
        // on top would be noise re-triggering every frame of the drag.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        use crate::view::UiParamSlot as ParamSlot;
        panel.sync_values(&mut tree, &[ParamSlot::exposed(50.0), ParamSlot::exposed(0.8)]);

        panel.drag.begin(ParamDragTarget::Param { index: 0 }, Vec2::ZERO);
        panel.sync_values(&mut tree, &[ParamSlot::exposed(75.0), ParamSlot::exposed(0.8)]);
        assert!(
            panel.value_flash[0].progress().is_none(),
            "no flash while this card is mid-drag"
        );
    }

    // ── P7.1 pinning tests — one per `ParamDragTarget` category, written
    // against the CURRENT six-slot `ParamDragState` before the
    // `DragController<ParamDragTarget>` fold (docs/UI_WIDGET_UNIFICATION_
    // DESIGN.md P7.1). Re-run green post-switch to prove the fold is a
    // lifecycle-only swap with byte-identical command emission.

    #[test]
    fn pinning_param_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let track = panel.slider_ids[0].as_ref().unwrap().track;
        let track_rect = tree.get_bounds(panel.slider_ids[0].as_ref().unwrap().track);
        let mid_x = track_rect.x + track_rect.width * 0.5;

        let down = panel.handle_pointer_down(track, Vec2::new(mid_x, track_rect.y), &tree);
        assert!(
            matches!(down.as_slice(), [PanelAction::ParamSnapshot(..), PanelAction::ParamChanged(..)]),
            "begin emits snapshot + first value: {down:?}"
        );
        assert!(panel.is_dragging());

        let quarter_x = track_rect.x + track_rect.width * 0.25;
        let moved = panel.handle_drag(Vec2::new(quarter_x, track_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::ParamChanged(target, pid, val)]
                if *target == GraphParamTarget::Effect(0) && pid.as_ref() == "radius" && (*val - 25.0).abs() < 1.0),
            "track emits the live value at the new position: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::ParamCommit(GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "end emits exactly one commit: {ended:?}"
        );
        assert!(!panel.is_dragging(), "drag slot cleared after end");
    }

    #[test]
    fn pinning_trim_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.driver_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let trim = panel.trim_ids[0].as_ref().expect("driver trim built");
        let min_bar_id = trim.min_bar_id;

        let down = panel.handle_pointer_down(min_bar_id, Vec2::new(0.0, 0.0), &tree);
        assert!(
            matches!(down.as_slice(), [PanelAction::TrimSnapshot(TrimKind::Driver, GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "begin emits a trim snapshot: {down:?}"
        );
        assert!(panel.is_dragging());

        let track_rect = tree.get_bounds(panel.slider_ids[0].as_ref().unwrap().track);
        let new_x = track_rect.x + track_rect.width * 0.4;
        let moved = panel.handle_drag(Vec2::new(new_x, track_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::TrimChanged(TrimKind::Driver, GraphParamTarget::Effect(0), pid, ..)] if pid.as_ref() == "radius"),
            "track emits the live trim range: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::TrimCommit(TrimKind::Driver, GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "end emits exactly one trim commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
    }

    /// BUG-257 regression: shift every node down (what `ScrollContainer::
    /// offset_content` does on a wheel scroll), then drag. The overlay nodes
    /// must land at the track's LIVE y, not the build-time cached one.
    fn scroll_shift(tree: &mut UITree, delta_y: f32) {
        for i in 0..tree.count() {
            let id = tree.id_at(i);
            let mut b = tree.get_bounds(id);
            b.y += delta_y;
            tree.set_bounds(id, b);
        }
    }

    #[test]
    fn trim_bars_follow_the_track_after_scroll() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.driver_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let track = panel.slider_ids[0].as_ref().unwrap().track;
        let trim = panel.trim_ids[0].as_ref().expect("driver trim built");
        let (min_bar, max_bar, fill) = (trim.min_bar_id, trim.max_bar_id, trim.fill_id);

        scroll_shift(&mut tree, 137.0);

        panel.handle_pointer_down(min_bar, Vec2::ZERO, &tree);
        let live = tree.get_bounds(track);
        let moved = panel.handle_drag(Vec2::new(live.x + live.width * 0.3, live.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::TrimChanged(TrimKind::Driver, ..)]),
            "trim drag still routes after scroll: {moved:?}"
        );

        for (name, id) in [("min_bar", min_bar), ("max_bar", max_bar), ("fill", fill)] {
            let y = tree.get_bounds(id).y;
            assert!(
                (y - live.y).abs() <= OVERLAY_INSET,
                "{name} y={y} should track the live track y={} (stale cache would put it ~137px up)",
                live.y
            );
        }
    }

    #[test]
    fn env_target_bar_follows_the_track_after_scroll() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let track = panel.slider_ids[0].as_ref().unwrap().track;
        let target_bar = panel.target_ids[0].as_ref().expect("envelope target built").target_bar_id;

        scroll_shift(&mut tree, 137.0);

        panel.handle_pointer_down(target_bar, Vec2::ZERO, &tree);
        let live = tree.get_bounds(track);
        let moved = panel.handle_drag(Vec2::new(live.x + live.width * 0.5, live.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::TargetChanged(..)]),
            "target drag still routes after scroll: {moved:?}"
        );

        let bar_y = tree.get_bounds(target_bar).y;
        assert!(
            (bar_y - (live.y - 2.0)).abs() < 0.01,
            "target bar y={bar_y} should sit 2px above the live track y={}",
            live.y
        );
    }

    #[test]
    fn pinning_env_target_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let target = panel.target_ids[0].as_ref().expect("envelope target built");
        let target_bar_id = target.target_bar_id;

        let down = panel.handle_pointer_down(target_bar_id, Vec2::new(0.0, 0.0), &tree);
        assert!(
            matches!(down.as_slice(), [PanelAction::TargetSnapshot(GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "begin emits a target snapshot: {down:?}"
        );
        assert!(panel.is_dragging());

        let track_rect = tree.get_bounds(panel.slider_ids[0].as_ref().unwrap().track);
        let new_x = track_rect.x + track_rect.width * 0.3;
        let moved = panel.handle_drag(Vec2::new(new_x, track_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::TargetChanged(GraphParamTarget::Effect(0), pid, norm)] if pid.as_ref() == "radius" && (*norm - 0.3).abs() < 0.05),
            "track emits the live target norm: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::TargetCommit(GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "end emits exactly one target commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
    }

    #[test]
    fn pinning_env_decay_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let cfg = panel.envelope_config_ids[0].as_ref().expect("envelope config built");
        let decay_track = cfg.decay_slider.track;
        let decay_rect = tree.get_bounds(cfg.decay_slider.track);

        let down = panel.handle_pointer_down(decay_track, Vec2::new(decay_rect.x, decay_rect.y), &tree);
        assert!(
            matches!(
                down.as_slice(),
                [PanelAction::EnvDecaySnapshot(GraphParamTarget::Effect(0), pid1), PanelAction::EnvDecayChanged(GraphParamTarget::Effect(0), pid2, _)]
                if pid1.as_ref() == "radius" && pid2.as_ref() == "radius"
            ),
            "begin emits snapshot + first decay value: {down:?}"
        );
        assert!(panel.is_dragging());

        let new_x = decay_rect.x + decay_rect.width * 0.6;
        let moved = panel.handle_drag(Vec2::new(new_x, decay_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::EnvDecayChanged(GraphParamTarget::Effect(0), pid, _)] if pid.as_ref() == "radius"),
            "track emits the live decay value: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::EnvDecayCommit(GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "end emits exactly one decay commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
    }

    /// Fixture with param 0's audio mod armed and Continuous — exercises the
    /// shaping sliders (Sensitivity/Attack/Release, `DrawerIds.sliders[0..3]`).
    fn effect_config_with_audio_shape_armed() -> ParamCardConfig {
        let mut c = effect_config();
        let n = c.params.len();
        c.audio.send_labels = vec!["Kick".into()];
        c.audio.send_ids = vec![manifold_foundation::AudioSendId::new("send-kick")];
        c.audio.active = vec![false; n];
        c.audio.send_id = vec![None; n];
        c.audio.kind_idx = vec![0; n];
        c.audio.band_idx = vec![0; n];
        c.audio.range_min = vec![0.0; n];
        c.audio.range_max = vec![1.0; n];
        c.audio.invert = vec![false; n];
        c.audio.rate = vec![false; n];
        c.audio.sensitivity = vec![1.0; n];
        c.audio.attack_ms = vec![5.0; n];
        c.audio.release_ms = vec![120.0; n];
        c.audio.trigger_mode_idx = vec![0; n];
        c.audio.action_idx = vec![0; n];
        c.audio.step_amount = vec![1.0; n];
        c.audio.wrap_idx = vec![0; n];
        c.audio.active[0] = true;
        c.audio.send_id[0] = Some(manifold_foundation::AudioSendId::new("send-kick"));
        c
    }

    #[test]
    fn pinning_audio_shape_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_audio_shape_armed());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (dids, _) = panel.audio_configs[0].as_ref().expect("audio drawer built");
        let sens_slider = dids.sliders[0]; // Sensitivity — the first shaping slider
        let sens_track = sens_slider.track;
        let sens_rect = tree.get_bounds(sens_track);

        let down = panel.handle_pointer_down(sens_track, Vec2::new(sens_rect.x, sens_rect.y), &tree);
        assert!(
            matches!(
                down.as_slice(),
                [PanelAction::AudioModShapeSnapshot(GraphParamTarget::Effect(0), pid1), PanelAction::AudioModShapeParamChanged(GraphParamTarget::Effect(0), pid2, AudioShapeParam::Sensitivity, _)]
                if pid1.as_ref() == "radius" && pid2.as_ref() == "radius"
            ),
            "begin emits snapshot + first shape value: {down:?}"
        );
        assert!(panel.is_dragging());

        let new_x = sens_rect.x + sens_rect.width * 0.7;
        let moved = panel.handle_drag(Vec2::new(new_x, sens_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::AudioModShapeParamChanged(GraphParamTarget::Effect(0), pid, AudioShapeParam::Sensitivity, _)] if pid.as_ref() == "radius"),
            "track emits the live shape value: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::AudioModShapeCommit(GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "end emits exactly one shape commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
    }

    #[test]
    fn pinning_step_amount_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        let mut cfg = effect_config_with_audio_shape_armed();
        cfg.audio.action_idx[0] = 1; // Step — the 4th drawer slider appears
        panel.configure(&cfg);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (dids, _) = panel.audio_configs[0].as_ref().expect("audio drawer built");
        let step_slider = *dids.sliders.get(3).expect("Step slider built while Action=Step");
        let step_track = step_slider.track;
        let step_rect = tree.get_bounds(step_track);

        let down = panel.handle_pointer_down(step_track, Vec2::new(step_rect.x, step_rect.y), &tree);
        assert!(
            matches!(
                down.as_slice(),
                [PanelAction::AudioModStepAmountSnapshot(GraphParamTarget::Effect(0), pid1), PanelAction::AudioModStepAmountChanged(GraphParamTarget::Effect(0), pid2, _)]
                if pid1.as_ref() == "radius" && pid2.as_ref() == "radius"
            ),
            "begin emits snapshot + first step value: {down:?}"
        );
        assert!(panel.is_dragging());

        let new_x = step_rect.x + step_rect.width * 0.8;
        let moved = panel.handle_drag(Vec2::new(new_x, step_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::AudioModStepAmountChanged(GraphParamTarget::Effect(0), pid, _)] if pid.as_ref() == "radius"),
            "track emits the live step value: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::AudioModStepAmountCommit(GraphParamTarget::Effect(0), pid)] if pid.as_ref() == "radius"),
            "end emits exactly one step commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
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
    fn card_collapse_tween_reserves_interpolated_height_clips_then_settles() {
        // P2 card collapse, end to end: expanded → collapsing (via the real
        // `configure()` round-trip a model-driven `EffectCollapseToggle`
        // takes) retargets the tween, a mid-flight build reserves an
        // interpolated height (so cards below it reflow) and clips the body
        // to it, and once the tween settles the card lays out at the fully-
        // collapsed height with no rows and no clip — same shape as the P1
        // drawer-tween test above, one level up (the whole card body instead
        // of one param's drawer).
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config()); // expanded; anim snapped to 1.0 (first configure)
        let expanded_h = panel.compute_height();

        let mut collapsed_cfg = effect_config();
        collapsed_cfg.collapsed = true;
        panel.configure(&collapsed_cfg); // Perform context, already configured once → eases
        assert!(panel.collapse_anim.is_animating(), "collapse retargets, doesn't snap");
        assert_eq!(
            panel.compute_height(),
            expanded_h,
            "still the expanded height the instant it retargets — data/UI-state \
             snaps instantly (is_collapsed is already true), only the visual eases"
        );

        panel.tick_drawers(color::MOTION_MED_MS * 0.5);
        let mid_h = panel.compute_height();
        assert!(mid_h < expanded_h, "mid-flight height has started shrinking: {mid_h} vs {expanded_h}");

        let mut tree = UITree::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));
        let clips_midflight = tree
            .nodes()
            .iter()
            .filter(|n| n.node_type == UINodeType::ClipRegion)
            .count();
        assert!(clips_midflight >= 1, "an animating card collapse builds its body under a clip region");

        for _ in 0..20 {
            panel.tick_drawers(20.0);
        }
        assert!(!panel.collapse_anim.is_animating(), "tween settles");
        let collapsed_h = panel.compute_height();
        assert!(collapsed_h < mid_h, "settled fully collapsed is smaller still: {collapsed_h} vs {mid_h}");

        let mut tree2 = UITree::new();
        panel.build(&mut tree2, Rect::new(0.0, 0.0, 280.0, 300.0));
        let clips_settled = tree2
            .nodes()
            .iter()
            .filter(|n| n.node_type == UINodeType::ClipRegion)
            .count();
        assert_eq!(clips_settled, 0, "settled collapsed card has no leftover clip region");
    }

    #[test]
    fn chevron_angle_matches_expanded_and_collapsed_endpoints() {
        // P2 "caret rotate": expanded (`collapse_frac() == 1.0`) sits at 0°;
        // fully collapsed (`collapse_frac() == 0.0`) rotates to -90°.
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config()); // expanded, first configure snaps
        assert_eq!(panel.chevron_angle(), 0.0, "expanded caret has no rotation");

        panel.set_collapsed(true); // snaps collapse_anim to 0.0 directly
        assert!(
            (panel.chevron_angle() - (-std::f32::consts::FRAC_PI_2)).abs() < 1e-6,
            "collapsed caret rotates -90 degrees, got {}",
            panel.chevron_angle()
        );
    }

    #[test]
    fn chevron_angle_tracks_collapse_anim_mid_flight_not_a_second_clock() {
        // The caret must ride the EXISTING `collapse_anim` tween, not a
        // separate animation — driving the tween partway must move the
        // angle partway between the two endpoints, and the built chevron
        // node's `UIStyle.transform` must reflect the same value `build()`
        // read at that moment.
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config()); // expanded; anim snapped to 1.0

        let mut collapsed_cfg = effect_config();
        collapsed_cfg.collapsed = true;
        panel.configure(&collapsed_cfg); // Perform context, already configured once → eases
        assert!(panel.collapse_anim.is_animating());

        panel.tick_drawers(color::MOTION_MED_MS * 0.5);
        let mid_angle = panel.chevron_angle();
        assert!(
            mid_angle < 0.0 && mid_angle > -std::f32::consts::FRAC_PI_2,
            "mid-flight caret angle sits strictly between the endpoints: {mid_angle}"
        );

        let mut tree = UITree::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));
        let chevron_id = panel.chevron_node_id().expect("chevron built");
        let transform = tree.get_node(chevron_id).unwrap().style.transform.expect("chevron carries a transform");
        // `Affine2::rotate` populates `b = sin(theta)`; recover theta and
        // compare against the panel's own mid-flight angle.
        assert!(
            (transform.b.asin() - mid_angle).abs() < 1e-4,
            "built chevron transform must match chevron_angle() at build time: {transform:?} vs {mid_angle}"
        );
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
    fn drawer_open_tween_reserves_interpolated_height_clips_then_settles() {
        // P1 drawer motion, end to end: closed → armed retargets the height tween,
        // the mid-flight build reserves an interpolated height (so content below
        // reflows) and clips the drawer to it, and once the tween settles the card
        // lays out at full height with no clip (byte-identical to the pre-motion
        // path).
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config()); // driver off → drawer closed, anim snapped to 0
        let closed_h = panel.compute_height();

        // Re-arm the driver on the same param: the tween retargets to the full
        // drawer height and starts easing (Perform context eases; the param existed
        // across both configures, so it's a set_target, not a snap).
        let mut armed = effect_config();
        armed.driver_active[0] = true;
        panel.configure(&armed);
        assert!(
            panel.drawer_height_anim[0].is_animating(),
            "arming a mod retargets the drawer tween → in flight"
        );
        let full_target = panel.row_drawer_height(0);
        assert!(full_target > 0.0);

        // Advance partway (well under MOTION_MED): reserved height sits strictly
        // between closed and fully open — the reflow tracks the tween.
        panel.tick_drawers(40.0);
        assert!(panel.drawer_height_anim[0].is_animating(), "still mid-flight after 40ms");
        let mid_h = panel.compute_height();
        assert!(
            mid_h > closed_h && mid_h < closed_h + full_target,
            "reserved height is interpolated: mid={mid_h} closed={closed_h} full={full_target}"
        );

        // The mid-flight build wraps the drawer in a clip region.
        let mut tree = UITree::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
        let clips_midflight = tree
            .nodes()
            .iter()
            .filter(|n| n.node_type == UINodeType::ClipRegion)
            .count();
        assert!(clips_midflight >= 1, "an animating drawer builds under a clip region");

        // Run the tween to completion → settled at full height, no drawer clip.
        for _ in 0..20 {
            panel.tick_drawers(20.0);
        }
        assert!(!panel.drawer_height_anim[0].is_animating(), "tween settles");
        assert!(
            (panel.compute_height() - (closed_h + full_target)).abs() < 0.1,
            "settled height = closed + full drawer contribution"
        );
        let mut tree2 = UITree::new();
        panel.build(&mut tree2, Rect::new(0.0, 0.0, 280.0, 400.0));
        let clips_settled = tree2
            .nodes()
            .iter()
            .filter(|n| n.node_type == UINodeType::ClipRegion)
            .count();
        assert!(
            clips_settled < clips_midflight,
            "settled build drops the drawer clip: settled={clips_settled} midflight={clips_midflight}"
        );
        assert!(panel.driver_config_ids[0].is_some(), "driver config built (unclipped) when settled");
    }

    // BUG-076 instrumentation: a card configured for the FIRST time (no prior
    // "closed" configure — the real "select a layer whose effects already
    // have armed audio/driver mods" case, not the toggle-open case
    // `drawer_open_tween_reserves_interpolated_height_clips_then_settles`
    // above exercises) must report its full settled height immediately —
    // `build` renders the armed drawer at full height with no "click to
    // open" step, so `compute_height` must agree without needing a
    // `tick_drawers` call first.
    #[test]
    fn configure_seeds_settled_height_when_drawer_already_armed_on_first_configure() {
        let mut armed = effect_config();
        armed.driver_active[0] = true;

        let mut panel = ParamCardPanel::new();
        panel.configure(&armed); // first-ever configure — drawer_height_anim starts empty

        assert!(
            !panel.drawer_height_anim[0].is_animating(),
            "a cold-armed drawer must be snapped, not eased, on first configure"
        );
        let full_target = panel.row_drawer_height(0);
        assert!(full_target > 0.0, "sanity: driver-armed row must reserve nonzero drawer height");

        // The regression this guards: before the fix, `compute_height()`
        // read `drawer_height_anim[0].value()` (0, since the tween hadn't
        // ticked) instead of the settled target — undercounting by
        // `full_target` on the very first frame the card is ever built.
        let height_before_any_tick = panel.compute_height();
        for _ in 0..20 {
            panel.tick_drawers(20.0);
        }
        let settled_height = panel.compute_height();
        assert!(
            (height_before_any_tick - settled_height).abs() < 0.1,
            "first-configure height ({height_before_any_tick}) must already equal the fully- \
             settled height ({settled_height}) — no undercounting before any tick"
        );
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
    fn tab_ink_targets_the_shown_tab_and_eases_on_switch() {
        // D1 "tab-ink slide": the ink underline's x-target follows whichever
        // tab is shown, and switching tabs retargets an eased tween rather
        // than snapping — same `AnimF32::set_target` contract `anim.rs`'s own
        // tests already prove; this checks the param-card wiring around it.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.driver_expanded[0] = true;
        panel.state.mod_state.envelope_expanded[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (driver_id, _) = panel.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Driver)
            .copied()
            .expect("driver tab present");
        let driver_x = tree.get_bounds(driver_id).x;
        // First build: the ink was "never positioned" (fresh AnimF32), so it
        // snapped straight to the Driver tab's x rather than sliding in.
        assert_eq!(panel.mod_tab_ink[0].value(), driver_x);
        assert!(!panel.mod_tab_ink[0].is_animating());

        let (env_id, _) = panel.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Envelope)
            .copied()
            .expect("envelope tab present");
        let env_x = tree.get_bounds(env_id).x;
        assert_ne!(env_x, driver_x, "the two tabs occupy different x positions");

        panel.handle_click(env_id);
        let mut tree2 = UITree::new();
        panel.build(&mut tree2, Rect::new(0.0, 0.0, 280.0, 400.0));
        // Retargeted, not re-snapped: value is still at (or near) the old
        // position the instant the target changes, and animating toward the
        // new one — exactly `set_target`'s "no jump on the same frame" rule.
        assert_eq!(panel.mod_tab_ink[0].target(), env_x);
        assert!(panel.mod_tab_ink[0].is_animating());

        for _ in 0..20 {
            panel.tick_drawers(20.0);
        }
        assert!((panel.mod_tab_ink[0].value() - env_x).abs() < 0.01, "settles at the new tab");
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
        // "3D Shading" icon (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) sits between
        // the ON/OFF toggle and the cog — but only when the feature is enabled
        // (`RELIGHT_FEATURE_ENABLED`). Disabled, the auto-gap row drops the icon
        // and its gap, so the toggle lands one slot left of the cog.
        let toggle_x = if RELIGHT_FEATURE_ENABLED {
            cog_x - GAP - RELIGHT_W - GAP - TOGGLE_W
        } else {
            cog_x - GAP - TOGGLE_W
        };
        let elem_y = inner_y + (HEADER_HEIGHT - 16.0) * 0.5;

        let close = |a: Rect, b: Rect| {
            (a.x - b.x).abs() < 0.01
                && (a.y - b.y).abs() < 0.01
                && (a.width - b.width).abs() < 0.01
                && (a.height - b.height).abs() < 0.01
        };
        let toggle = tree.get_bounds(panel.host.node_id_for_key(KEY_TOGGLE).unwrap());
        assert!(close(toggle, Rect::new(toggle_x, elem_y, TOGGLE_W, 16.0)), "toggle {toggle:?}");
        if RELIGHT_FEATURE_ENABLED {
            let relight_x = cog_x - GAP - RELIGHT_W;
            let relight = tree.get_bounds(panel.host.node_id_for_key(KEY_RELIGHT).unwrap());
            assert!(close(relight, Rect::new(relight_x, elem_y, RELIGHT_W, 16.0)), "relight {relight:?}");
        } else {
            assert!(panel.host.node_id_for_key(KEY_RELIGHT).is_none(), "relight icon hidden when feature off");
        }
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
        // "3D Shading" icon (`docs/DEPTH_RELIGHT_DESIGN.md` P5b) sits between
        // Change and the cog — but only when the feature is enabled
        // (`RELIGHT_FEATURE_ENABLED`). Disabled, the icon and its leading gap are
        // both dropped, so Change lands one slot left of the cog.
        let change_x = if RELIGHT_FEATURE_ENABLED {
            cog_x - GAP - RELIGHT_W - GAP - CHANGE_BTN_W
        } else {
            cog_x - GAP - CHANGE_BTN_W
        };

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
        if RELIGHT_FEATURE_ENABLED {
            let relight_x = cog_x - GAP - RELIGHT_W;
            let relight = tree.get_bounds(panel.host.node_id_for_key(KEY_RELIGHT).unwrap());
            assert!(
                close(relight, Rect::new(relight_x, inner_y, RELIGHT_W, HEADER_HEIGHT)),
                "relight {relight:?}"
            );
        } else {
            assert!(panel.host.node_id_for_key(KEY_RELIGHT).is_none(), "relight icon hidden when feature off");
        }
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
            // Real id (fixed 2026-07-11) — a populated generator card carries
            // `inst.id` now, never the blanked `EffectId::new("")` that used
            // to break its fire-meter key lookups; this fixture models a
            // populated card, not the zero-param `empty_generator_config`
            // fallback (which is the one case that legitimately stays blank).
            effect_id: EffectId::new("gen-plasma-1"),
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
                    is_trigger_gate: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                    section: None,
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
                    is_trigger_gate: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                    section: None,
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
                    is_trigger_gate: false,
                    value_labels: None,
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                    section: None,
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
            automation_active: vec![false; 3],
            automation_overridden: vec![false; 3],
            relight: RelightCardConfig::default(),
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

    /// D5 card sections (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): Speed +
    /// Invert sectioned under "Leaf" (a contiguous run), Scale unsectioned.
    fn gen_config_with_sections() -> ParamCardConfig {
        let mut c = gen_config();
        c.params[0].section = Some("Leaf".to_string());
        c.params[1].section = Some("Leaf".to_string());
        c.params[2].section = None;
        c
    }

    #[test]
    fn section_runs_groups_contiguous_same_section_rows() {
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config_with_sections());
        let runs = panel.section_runs();
        assert_eq!(
            runs,
            vec![(0, 2, Some("Leaf".to_string())), (2, 1, None)],
            "one run for the contiguous Leaf pair, one for the trailing unsectioned row"
        );
    }

    #[test]
    fn build_generator_draws_one_header_for_a_sectioned_run_and_none_for_unsectioned() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config_with_sections());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert_eq!(panel.section_header_ids.len(), 1, "exactly one header — the Leaf run");
        assert_eq!(panel.section_header_ids[0].1, "Leaf");
        // Every row still builds (unfolded by default) — Speed/Invert/Scale
        // widgets all present, same as the no-sections case.
        assert!(panel.slider_ids[0].is_some());
        assert!(panel.toggle_ids[1].is_some());
        assert!(panel.slider_ids[2].is_some());
    }

    #[test]
    fn clicking_a_section_header_folds_it_and_a_rebuild_skips_its_rows() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config_with_sections());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let header_id = panel.section_header_ids[0].0;
        let actions = panel.handle_click(header_id);
        assert!(
            matches!(actions.as_slice(), [PanelAction::SectionFoldToggled]),
            "fold click is UI-local — no model-mutating action"
        );
        assert_eq!(
            panel.section_folded.get("Leaf"),
            Some(&true),
            "handle_click flips this panel's own fold state"
        );

        // Rebuild on a fresh tree (a new frame) with the section now folded:
        // the Leaf run's rows (Speed, Invert) are skipped entirely; the
        // header still draws (so it can be clicked again to unfold); Scale
        // (unsectioned, outside the run) is unaffected. `section_folded`
        // survives the rebuild — the same "preserved across rebuilds"
        // convention `mod_active_tab`/`drawer_height_anim` already use.
        let mut tree2 = UITree::new();
        panel.build(&mut tree2, Rect::new(0.0, 0.0, 280.0, 300.0));
        assert_eq!(panel.section_header_ids.len(), 1, "the header itself still draws while folded");
        assert!(panel.slider_ids[0].is_none(), "Speed's row was skipped (folded)");
        assert!(panel.toggle_ids[1].is_none(), "Invert's row was skipped (folded)");
        assert!(panel.slider_ids[2].is_some(), "Scale (outside the folded run) still builds");

        // Click again — unfolds.
        let actions = panel.handle_click(header_id);
        assert!(matches!(actions.as_slice(), [PanelAction::SectionFoldToggled]));
        assert_eq!(panel.section_folded.get("Leaf"), Some(&false));
        let mut tree3 = UITree::new();
        panel.build(&mut tree3, Rect::new(0.0, 0.0, 280.0, 300.0));
        assert!(panel.slider_ids[0].is_some(), "Speed's row is back once unfolded");
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
            PanelAction::ParamToggle(target, param_id) => {
                assert_eq!(*target, GraphParamTarget::Generator);
                assert_eq!(param_id.as_ref(), "invert");
            }
            other => panic!("expected ParamToggle, got {:?}", other),
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
        // Drawer height includes the post-drawer break (DRAWER_BOTTOM_GAP).
        assert!((expanded_h - base_h - driver_config_height() - DRAWER_BOTTOM_GAP).abs() < 0.1);
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
        let audio_h = crate::panels::param_slider_shared::audio_config_height(
            &panel.param_info[0],
            &panel.state.mod_state,
            0,
            false,
        );
        // Drawer height includes the post-drawer break (DRAWER_BOTTOM_GAP).
        assert!((expanded_h - base_h - audio_h - DRAWER_BOTTOM_GAP).abs() < 0.1);
    }

    #[test]
    fn right_click_on_param_track_resolves_to_slider_reset_with_declared_default() {
        // BUG-061: the param reset now rides the generic SliderReset trio (the
        // old per-panel right-click reset action was deleted), carrying the
        // param's own declared default (10.0 for "radius" here).
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let track = panel.slider_ids[0].as_ref().unwrap().track;
        match reg.resolve(&tree, Some(track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::SliderReset { changed, .. }) => {
                assert!(matches!(*changed, PanelAction::ParamChanged(_, _, v) if (v - 10.0).abs() < f32::EPSILON));
            }
            other => panic!("expected SliderReset, got {other:?}"),
        }
    }

    #[test]
    fn right_click_on_audio_shape_slider_resolves_to_slider_reset_with_shape_default() {
        // BUG-061: the drawer's Amount/Attack/Release shaping sliders never had
        // a reset gesture before this — each must resolve to AudioModShape's
        // own default (1.0 / 5ms / 120ms), not the current live value.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.state.mod_state.audio_active[0] = true;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let dids = &panel.audio_configs[0].as_ref().expect("audio drawer built").0;
        assert_eq!(dids.sliders.len(), 3, "Amount/Attack/Release");

        let expected = [
            (AudioShapeParam::Sensitivity, AUDIO_SENS_DEFAULT),
            (AudioShapeParam::Attack, AUDIO_ATTACK_DEFAULT_MS),
            (AudioShapeParam::Release, AUDIO_RELEASE_DEFAULT_MS),
        ];
        for (si, (which, default)) in expected.into_iter().enumerate() {
            let track = dids.sliders[si].track;
            match reg.resolve(&tree, Some(track), crate::intent::Gesture::RightClick) {
                Some(PanelAction::SliderReset { changed, .. }) => match *changed {
                    PanelAction::AudioModShapeParamChanged(_, _, got_which, v) => {
                        assert_eq!(got_which, which);
                        assert!((v - default).abs() < f32::EPSILON, "slider {si}: {v} != {default}");
                    }
                    other => panic!("slider {si}: expected AudioModShapeParamChanged, got {other:?}"),
                },
                other => panic!("slider {si}: expected SliderReset, got {other:?}"),
            }
        }
    }

    /// Shared assertion: `track` resolves to a `SliderReset` via the registry
    /// on right-click. Reused across the main-slider and drawer-slider cases
    /// below (spec §8).
    fn assert_track_resets(reg: &crate::intent::IntentRegistry, tree: &UITree, track: NodeId) {
        match reg.resolve(tree, Some(track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::SliderReset { .. }) => {}
            other => panic!("expected SliderReset on track {track:?}, got {other:?}"),
        }
    }

    #[test]
    fn trigger_gate_drawer_sliders_all_resolve_to_slider_reset() {
        // BUG-070: the Clip Trigger drawer's Amount/Attack/Release sliders got
        // NO reset gesture, because register_intents' old per-row loop bailed
        // at `let Some(ids) = slider else { continue }` before it ever reached
        // the drawer — a trigger-gate row has no main slider (confirmed by the
        // `slider_ids[gi].is_none()` assertion in
        // `build_effect_trigger_gate_row_and_drawer` above), so the drawer's
        // Amount/Attack/Release sliders were structurally unreachable from that
        // loop. This test fails on that old code path (register_intents never
        // reaches `audio_configs[gi]` for this row) and passes once resets are
        // replayed independent of whether the row has a main slider — the fix
        // in this file's `register_intents`.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_trigger_gate());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let gi = panel.param_info.len() - 1;
        assert!(panel.slider_ids[gi].is_none(), "trigger-gate row has no main slider");

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let dids = &panel.audio_configs[gi].as_ref().expect("audio drawer armed in fixture").0;
        assert_eq!(dids.sliders.len(), 3, "Amount/Attack/Release");
        for sl in &dids.sliders {
            assert_track_resets(&reg, &tree, sl.track);
        }
    }

    #[test]
    fn normal_param_row_main_slider_track_resolves_to_slider_reset() {
        // Companion to the trigger-gate coverage test above, using the same
        // shared helper: a plain slider row's main track still resolves too
        // (unchanged behaviour — now via the replay pass instead of the
        // deleted in-loop registration).
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let track = panel.slider_ids[0].as_ref().unwrap().track;
        assert_track_resets(&reg, &tree, track);
    }
}
