//! Unified parameter-card data contract shared by the effect and generator
//! inspector cards.
//!
//! Effects and generators present the same instrument to the user — a card
//! with a header, a column of parameter rows (each a slider plus optional
//! driver / envelope / Ableton modulation drawers), and a few kind-specific
//! affordances. Historically each side carried its own `…ParamRow` /
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

use crate::{ParamsAction, RootAction};
use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_slider_shared::*;
use super::{
    AudioShapeParam, GraphParamTarget, PanelAction, ScrubPhase, ScrubValue, TrimKind,
    UiRelightField, UiRelightHeightFrom, ValueRef,
};
use crate::anim::{AnimF32, Transient};
use crate::chrome::{Align, ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::node::*;
use crate::param_surface::{CatalogAffordance, CatalogSurface, ParamRow, ParamSurface, RowRole};
use crate::slider::{BitmapSlider, SliderColors, TrackSpan};
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

mod render;
mod routing;
mod state;

pub use state::{
    CardContext, ParamCardKind, ParamCardState, ParamCardStringInfo, RelightCardConfig, RowMod,
};
pub(crate) use state::RELIGHT_FIELD_SPECS;

// ── ParamCardPanel ───────────────────────────────────────────────

/// The unified inspector parameter card. One struct presents both effect cards
/// and generator cards; [`kind`](ParamCardKind) selects the shell furniture
/// (effect: drag-handle + badges + ON/OFF toggle + hierarchical parenting;
/// generator: Change button + toggle/trigger/string rows + flat parenting)
/// while the per-parameter row core — slider + trim/target/range handles + D/E
/// buttons + driver/envelope/Ableton drawers — is shared verbatim via
/// [`build_param_row`] / `row_action` (P2, `docs/WIDGET_TREE_DESIGN.md`). The drag-move and
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
    rows: Vec<ParamRow>,
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
    /// The per-row id-bundle machinery + reverse `WidgetId → (row, role)`
    /// index (P-S2): every slider/trim/target/driver/envelope/audio/toggle/
    /// mapping/section node id this card's rows mint, plus `row_index`. The
    /// row builders populate its fields, then `reindex_row` folds them into
    /// the index; `handle_click`/`handle_pointer_down`/`register_intents`
    /// resolve through it. The per-row *data* (rows, `mod_state`, the value /
    /// osc / tab caches below) stays on the panel and is passed to
    /// `RowHost::row_action` by reference.
    row_host: RowHost,
    /// Per-param base (pre-modulation) value, cached each sync so a value-cell
    /// double-click prefills the type-in box with the user-set value, not the
    /// live modulated display. Sized to the param count in `configure`.
    base_values: Vec<f32>,
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
    string_param_btn_ids: Vec<Option<NodeId>>,

    // Per-param OSC addresses (for click-to-copy). Indexed by param index.
    osc_addresses: Vec<Option<String>>,

    /// D5 card-section fold state (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md
    /// §2), keyed by section name. UI-local workspace state — same home as
    /// the graph canvas's `GraphCanvas::collapsed` (survives rebuilds of
    /// this panel instance, never serialized to the project; folds reset on
    /// app restart, persistence Deferred). Missing entry = unfolded
    /// (default). A section not present in the current `rows` is
    /// simply never consulted — no pruning needed.
    section_folded: ahash::AHashMap<String, bool>,

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
            rows: Vec::new(),
            string_param_info: Vec::new(),
            row_host: RowHost::new(),
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
            base_values: Vec::new(),
            mod_active_tab: Vec::new(),
            drawer_height_anim: Vec::new(),
            mod_tab_ink: Vec::new(),
            compact: false,
            string_param_btn_ids: Vec::new(),
            osc_addresses: Vec::new(),
            section_folded: ahash::AHashMap::new(),
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
        }
    }

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

    /// The card ROOT's identity key (D4): cards are siblings under the
    /// inspector column, so the root's `View::key` — which now pins the
    /// durable WidgetId — must be the card's stable identity, never a
    /// shared constant. Effect instances key on their `EffectId`; generator
    /// cards on their layer (one generator card per layer scope).
    fn identity_key(&self) -> u64 {
        match self.kind {
            ParamCardKind::Effect => crate::param_surface::stable_key(self.effect_id.as_str()),
            ParamCardKind::Generator => match &self.layer_id {
                Some(lid) => crate::param_surface::stable_key(&format!("gen:{lid}")),
                None => crate::param_surface::stable_key("gen:editor"),
            },
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
        for (pi, slot) in self.row_host.slider_ids.iter().enumerate() {
            let Some(ids) = slot else { continue };
            if ids.value_text != node_id {
                continue;
            }
            let info = self.rows.get(pi)?;
            if info.spec.value_labels.is_some() {
                return None;
            }
            return Some(PanelAction::Root(RootAction::BeginParamTextInput {
                target: self.param_target(),
                param_id: self.rows[pi].id.clone(),
                anchor: tree.get_bounds(ids.value_text),
                value: self.base_values.get(pi).copied().unwrap_or(info.spec.default),
                min: info.spec.min,
                max: info.spec.max,
                whole_numbers: info.spec.whole_numbers,
            }));
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
            &self.row_host.driver_config_ids,
        )?;
        Some(PanelAction::Root(RootAction::BeginDriverPeriodTextInput {
            target: self.param_target(),
            param_id: self.rows[pi].id.clone(),
            anchor: tree.get_bounds(node_id),
            value: self.state.mod_state.driver_effective_period(pi),
        }))
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
        for (pi, cfg) in self.row_host.audio_configs.iter().enumerate() {
            let Some((dids, _)) = cfg else { continue };
            let Some(info) = self.rows.get(pi) else { continue };
            let Some(Some(meter)) = dids.meters.first() else { continue };
            let key = manifold_foundation::fire_meter_key_for_param(
                self.effect_id.as_str(),
                info.id.as_ref(),
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
        self.row_host.audio_configs.iter().enumerate().find_map(|(pi, cfg)| {
            cfg.as_ref()?;
            let info = self.rows.get(pi)?;
            info.spec.is_trigger_gate.then_some(pi)
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

    /// Live, scroll-current, animation-current bounds of this card's full
    /// frame — the border node's rect. `None` if the card has never been
    /// built (no `border_id`) or the node has since gone stale. This is a
    /// read-through into `UITree`, not a cache: nothing is stored on
    /// `ParamCardPanel` here. Use for hit-testing (e.g. drag);
    /// `compute_height()` is a build-time/animated value that goes stale
    /// under in-place scroll (`ScrollContainer::offset_content`).
    pub fn live_bounds(&self, tree: &UITree) -> Option<Rect> {
        Some(tree.get_bounds(self.border_id?))
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }

    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// D9 widget catalog — enumerate this card's sanctioned row affordances by
    /// walking its own node range and resolving each interactive node through
    /// the SAME [`RowIndex`](crate::param_surface::RowIndex) routing uses,
    /// pairing the owning row's durable id + [`RowRole`] with the node's
    /// durable `WidgetId` + queryable `name_of` name (the two facts the tree
    /// dump already serializes). This is the enumeration VIEW, not a new
    /// protocol: identity comes from the widget salt and the name, exactly as
    /// in the dump; the catalog only regroups them per row and adds the role.
    ///
    /// A `name == None` entry is a nameless sanctioned affordance surfaced (the
    /// BUG-239 shape) — never invented here. Non-live cards (no nodes built
    /// this frame) yield an empty affordance list. Call after `build()`.
    pub fn catalog(&self, tree: &UITree) -> CatalogSurface {
        let mut affordances = Vec::new();
        if self.is_live() {
            let end = self.first_node.saturating_add(self.node_count);
            for idx in self.first_node..end {
                let node_id = tree.id_at(idx);
                let widget = tree.widget_of(node_id);
                if let Some((row, role)) = self.row_host.row_index.get(widget) {
                    affordances.push(CatalogAffordance {
                        row_id: self.rows[row].id.to_string(),
                        role,
                        widget: widget.raw(),
                        name: tree.name_of(node_id).map(str::to_string),
                    });
                }
            }
        }
        CatalogSurface { kind: self.kind, title: self.name.clone(), affordances }
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
    pub(crate) fn matches_effect_config(&self, config: &ParamSurface) -> bool {
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
        self.rows
            .iter()
            .find(|p| p.id == param_id)
            .is_some_and(|p| p.mapping.ableton_display.is_some())
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
        let pi = self.rows.iter().position(|p| p.id == param_id)?;
        let cid = (*self.row_host.mapping_chevron_ids.get(pi)?)?;
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
        for (i, info) in self.rows.iter().enumerate() {
            let label_id = self.row_host
                .slider_ids
                .get(i)
                .and_then(|s| s.as_ref())
                .and_then(|ids| ids.label)
                .or_else(|| {
                    self.row_host.toggle_ids
                        .get(i)
                        .and_then(|t| t.as_ref())
                        .and_then(|ids| ids.label_id)
                });
            if let Some(lid) = label_id
                && tree.get_bounds(lid).contains(pos)
            {
                return Some(info.id.clone());
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
        let i = self.rows.iter().position(|p| p.id == param_id)?;
        let label_id = self.row_host
            .slider_ids
            .get(i)
            .and_then(|s| s.as_ref())
            .and_then(|ids| ids.label)
            .or_else(|| {
                self.row_host.toggle_ids
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
        &self.row_host.section_header_ids
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
    // `ModulationAction` is asserted against only in these tests now — the
    // production references moved to `RowHost::row_action` (P-S2).
    use crate::ModulationAction;
    use crate::param_surface::{RowMapping, RowSpec, RowValue};
    use crate::tree::UITree;

    // ── Effect-card fixtures + tests ──────────────────────────────

    fn effect_config() -> ParamSurface {
        ParamSurface {
            kind: ParamCardKind::Effect,
            effect_index: 0,
            effect_id: EffectId::new("test-effect-0"),
            title: "Blur".into(),
            enabled: true,
            collapsed: false,
            supports_envelopes: true,
            string_params: Vec::new(),
            layer_id: None,
            rows: vec![
                ParamRow {
                    id: std::borrow::Cow::Borrowed("radius"),
                    spec: RowSpec {
                        name: "Radius".into(),
                        min: 0.0,
                        max: 100.0,
                        default: 10.0,
                        whole_numbers: true,
                        is_angle: false,
                        is_toggle: false,
                        is_trigger: false,
                        is_trigger_gate: false,
                        value_labels: None,
                        section: None,
                    },
                    value: RowValue { base: 10.0, effective: 10.0, exposed: true, driven: false },
                    modulation: RowMod::default(),
                    mapping: RowMapping {
                        osc_address: None,
                        ableton_display: None,
                        ableton_range: None,
                        mappable: false,
                    },
                },
                ParamRow {
                    id: std::borrow::Cow::Borrowed("strength"),
                    spec: RowSpec {
                        name: "Strength".into(),
                        min: 0.0,
                        max: 1.0,
                        default: 0.5,
                        whole_numbers: false,
                        is_angle: false,
                        is_toggle: false,
                        is_trigger: false,
                        is_trigger_gate: false,
                        value_labels: None,
                        section: None,
                    },
                    value: RowValue { base: 0.5, effective: 0.5, exposed: true, driven: false },
                    modulation: RowMod::default(),
                    mapping: RowMapping {
                        osc_address: None,
                        ableton_display: None,
                        ableton_range: None,
                        mappable: false,
                    },
                },
            ],
            has_graph_mod: false,
            audio: Default::default(),
            relight: RelightCardConfig::default(),
        }
    }

    /// Config with a third (`is_toggle`) and fourth (`is_trigger`) param —
    /// exercises the effect card's toggle/trigger row rendering + click
    /// dispatch (§8.4 P3b: effect cards previously had no branch for either
    /// and rendered them as raw sliders — the Task A bug).
    fn effect_config_with_toggle_and_trigger() -> ParamSurface {
        let mut c = effect_config();
        c.rows.push(ParamRow {
            id: std::borrow::Cow::Borrowed("invert"),
            spec: RowSpec {
                name: "Invert".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                whole_numbers: false,
                is_angle: false,
                is_toggle: true,
                is_trigger: false,
                is_trigger_gate: false,
                value_labels: None,
                section: None,
            },
            value: RowValue { base: 0.0, effective: 0.0, exposed: true, driven: false },
            modulation: RowMod::default(),
            mapping: RowMapping {
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: false,
            },
        });
        c.rows.push(ParamRow {
            id: std::borrow::Cow::Borrowed("reset"),
            spec: RowSpec {
                name: "Reset".into(),
                min: 0.0,
                max: 0.0,
                default: 0.0,
                whole_numbers: true,
                is_angle: false,
                is_toggle: false,
                is_trigger: true,
                is_trigger_gate: false,
                value_labels: None,
                section: None,
            },
            value: RowValue { base: 0.0, effective: 0.0, exposed: true, driven: false },
            modulation: RowMod::default(),
            mapping: RowMapping {
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: false,
            },
        });
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
        assert!(panel.row_host.slider_ids[0].is_some()); // Radius = slider
        assert!(panel.row_host.slider_ids[1].is_some()); // Strength = slider
        assert!(panel.row_host.slider_ids[2].is_none()); // Invert = toggle, no slider
        assert!(panel.row_host.slider_ids[3].is_none()); // Reset = trigger, no slider
        assert!(panel.row_host.toggle_ids[2].is_some());
        assert!(panel.row_host.toggle_ids[3].is_some());

        // Task B (D5b): the trigger row reaches the audio-mod "A" button;
        // the toggle row does not (zero D/E/A lane, unchanged rule).
        assert!(panel.row_host.audio_btn_ids[2].is_none());
        assert!(panel.row_host.audio_btn_ids[3].is_some());
    }

    /// `WIDGET_TREE_DESIGN.md` §5 dump-queryability: every converged card row
    /// carries a param-id-derived automation name on its row-root and drivable
    /// controls, so a `--script` flow can find and drive it directly. This is
    /// the wire VD-035 needed — a modifier param row past its value cell is
    /// unreachable by `under_text` (the nearest preceding texted sibling is the
    /// value cell, not the label), so the row's OWN name must be its selector.
    #[test]
    fn param_rows_carry_queryable_names() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        // Slider row ("radius"): row-root + track + value cell + driver button.
        let rc = panel.row_host.row_catcher_ids[0].expect("radius row has a row catcher");
        assert_eq!(tree.name_of(rc), Some("param_row.radius"));
        let slider = panel.row_host.slider_ids[0].expect("radius row has a slider");
        assert_eq!(tree.name_of(slider.track), Some("param_row.radius.slider"));
        assert_eq!(tree.name_of(slider.value_text), Some("param_row.radius.value"));
        let drv = panel.row_host.driver_btn_ids[0].expect("radius row has a driver button");
        assert_eq!(tree.name_of(drv), Some("param_row.radius.driver_btn"));

        // Toggle row ("invert"): no separate row-catcher — its button IS the
        // row's identity, so the row name lands there.
        let toggle = panel.row_host.toggle_ids[2].as_ref().expect("invert row is a toggle");
        assert_eq!(tree.name_of(toggle.button_id), Some("param_row.invert"));
    }

    /// D9 widget-catalog self-test — the enumeration view proves out, and the
    /// BUG-239 structural kill holds: the catalog enumerates every sanctioned
    /// row affordance with its durable id + role + queryable name, and NO row
    /// can appear without at least one queryable name (a nameless row would be
    /// undriveable by a flow harness — the exact BUG-239 gap this closes).
    #[test]
    fn catalog_enumerates_rows_roles_names_and_no_row_is_nameless() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let cat = panel.catalog(&tree);
        assert_eq!(cat.kind, ParamCardKind::Effect);
        assert_eq!(cat.title, "Blur");
        assert!(!cat.affordances.is_empty(), "a live card must enumerate affordances");

        // Enumeration proves out: the slider row's track/value/driver arm and
        // the toggle row's button appear with their exact durable roles + names
        // (the same names `param_rows_carry_queryable_names` asserts on-node —
        // the catalog surfaces them by enumeration, not a second source).
        let has = |row: &str, role: RowRole, name: &str| {
            cat.affordances
                .iter()
                .any(|a| a.row_id == row && a.role == role && a.name.as_deref() == Some(name))
        };
        assert!(has("radius", RowRole::Slider, "param_row.radius.slider"));
        assert!(has("radius", RowRole::Slider, "param_row.radius.value"));
        assert!(has("radius", RowRole::DriverBtn, "param_row.radius.driver_btn"));
        assert!(has("radius", RowRole::RowCatcher, "param_row.radius"));
        assert!(has("invert", RowRole::ToggleBtn, "param_row.invert"));

        // Every enumerated affordance's durable WidgetId is non-zero and its
        // row_id is a real row — the catalog can't manufacture an entry.
        let known: std::collections::BTreeSet<&str> =
            ["radius", "strength", "invert", "reset"].into_iter().collect();
        for a in &cat.affordances {
            assert!(a.widget != 0, "affordance carries a durable WidgetId");
            assert!(known.contains(a.row_id.as_str()), "enumerated row {} is a real row", a.row_id);
        }

        // ── The BUG-239 structural kill ──────────────────────────────────
        // Every ROW the catalog surfaces carries at least one queryable name.
        // A row with only `name == None` affordances would be a row a flow
        // harness cannot address — precisely the class D9 forecloses. If a
        // future edit ships such a row, THIS assertion goes red.
        let rows: std::collections::BTreeSet<String> =
            cat.affordances.iter().map(|a| a.row_id.clone()).collect();
        for rid in &rows {
            assert!(
                cat.affordances.iter().any(|a| &a.row_id == rid && a.name.is_some()),
                "row `{rid}` has no queryable name on ANY affordance — BUG-239 structural regression"
            );
        }
        // All four rows are present (nothing silently dropped).
        assert_eq!(rows, known.iter().map(|s| s.to_string()).collect());
    }

    #[test]
    fn click_toggle_param_effect() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let button_id = panel.row_host.toggle_ids[2].as_ref().unwrap().button_id;
        let actions = panel.handle_click(button_id, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Params(ParamsAction::ParamToggle(target, param_id)) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "invert");
            }
            other => panic!("expected ParamToggle, got {:?}", other),
        }
    }

    #[test]
    fn click_trigger_param_effect() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_toggle_and_trigger());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let button_id = panel.row_host.toggle_ids[3].as_ref().unwrap().button_id;
        let actions = panel.handle_click(button_id, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Params(ParamsAction::ParamFire(target, param_id)) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "reset");
            }
            other => panic!("expected ParamFire, got {:?}", other),
        }

        // The trigger row's "A" button reaches the shared audio-mod dispatch.
        let audio_btn = panel.row_host.audio_btn_ids[3].unwrap();
        let actions = panel.handle_click(audio_btn, &tree);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::Root(RootAction::OpenAudioSetup) | PanelAction::Modulation(ModulationAction::AudioModToggle(..))));
    }

    /// Config with an `is_trigger_gate` toggle param (§9, the outer-card gate
    /// for a generator's/effect's audio trigger response — Strobe's/the 11
    /// generators' `clip_trigger`), armed with a real `ParameterAudioMod` (a
    /// `trigger_mode`, not a separate config type) so the drawer builds.
    /// Exercises `build_toggle_trigger_row`'s `is_trigger_gate` branch riding
    /// the SAME standard audio-mod drawer `effect_config_with_toggle_and_
    /// trigger`'s `is_trigger` (D5b) coverage above exercises, plus the
    /// trailing Mode row.
    fn effect_config_with_trigger_gate() -> ParamSurface {
        let mut c = effect_config();
        c.rows.push(ParamRow {
            id: std::borrow::Cow::Borrowed("clip_trigger"),
            spec: RowSpec {
                name: "Clip Trigger".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                whole_numbers: false,
                is_angle: false,
                is_toggle: true,
                is_trigger: false,
                is_trigger_gate: true,
                value_labels: None,
                section: None,
            },
            value: RowValue { base: 0.0, effective: 0.0, exposed: true, driven: false },
            modulation: RowMod::default(),
            mapping: RowMapping {
                osc_address: None,
                ableton_display: None,
                ableton_range: None,
                mappable: false,
            },
        });
        let n = c.rows.len();

        c.audio.send_labels = vec!["Kick".into()];
        c.audio.send_ids = vec![manifold_foundation::AudioSendId::new("send-kick")];
        c.audio.rows = vec![AudioRowState::default(); n];
        let gi = n - 1; // the clip_trigger row's index
        c.audio.rows[gi].active = true;
        c.audio.rows[gi].send_id = Some(manifold_foundation::AudioSendId::new("send-kick"));
        c.audio.rows[gi].band_idx = 1; // Low
        c.audio.rows[gi].sensitivity = 0.65;
        c.audio.rows[gi].trigger_mode_idx = 2; // Both
        c
    }

    #[test]
    fn build_effect_trigger_gate_row_and_drawer() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_trigger_gate());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let gi = panel.rows.len() - 1;
        // Renders as a toggle row (not a slider), same as a plain toggle —
        // but ALSO reaches the standard audio-mod "A" button + drawer, which
        // a plain toggle never does.
        assert!(panel.row_host.slider_ids[gi].is_none());
        assert!(panel.row_host.toggle_ids[gi].is_some());
        assert!(panel.row_host.audio_btn_ids[gi].is_some());
        // Armed in the fixture (`active[gi] = true`) — the drawer must build.
        assert!(panel.row_host.audio_configs[gi].is_some());
        // The collapsed-row mode badge exists (mode = Both, index 2 > 0).
        assert!(panel.row_host.audio_trigger_mode_badge_ids[gi].is_some());
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
        let gi = cfg.rows.len() - 1;
        cfg.audio.rows[gi].active = false; // disarmed — drawer never builds
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
        let gi = cfg.rows.len() - 1;
        // Same armed row, reshaped into a plain continuous (non-toggle,
        // non-trigger) param — a genuine non-gate shape (not just a flag flip
        // on the toggle-row fixture), which still shows an Amount meter but
        // must never re-tap the scope send/band.
        cfg.rows[gi].spec.is_trigger_gate = false;
        cfg.rows[gi].spec.is_toggle = false;
        panel.configure(&cfg);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        assert!(panel.row_host.audio_configs[gi].is_some(), "sanity: the drawer still builds");
        assert_eq!(panel.open_fire_mode_drawer_send(), None);
        assert_eq!(panel.open_fire_mode_drawer_band(), None);
    }

    #[test]
    fn click_trigger_gate_drawer_effect() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_trigger_gate());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
        let gi = panel.rows.len() - 1;

        // The "A" button toggles the mod (armed → disarm, since the fixture
        // starts active) through the SAME `AudioModToggle` every other
        // audio-mod row uses.
        let audio_btn = panel.row_host.audio_btn_ids[gi].unwrap();
        let actions = panel.handle_click(audio_btn, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Modulation(ModulationAction::AudioModToggle(target, param_id)) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "clip_trigger");
            }
            other => panic!("expected AudioModToggle, got {:?}", other),
        }

        // The drawer's Source (send) button — flat index 0 (only one send).
        // Clone the button ids out first: `handle_click` needs `&mut panel`,
        // which would otherwise conflict with the borrow of `dids`.
        let (dids, send_count) = panel.row_host.audio_configs[gi].as_ref().unwrap();
        assert_eq!(*send_count, 1);
        let button_ids: Vec<NodeId> = dids.button_ids().to_vec();
        let send_btn = button_ids[0];
        let actions = panel.handle_click(send_btn, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Modulation(ModulationAction::AudioModSetSource(target, param_id, send_id, _feature)) => {
                assert_eq!(*target, GraphParamTarget::Effect(0));
                assert_eq!(param_id.as_ref(), "clip_trigger");
                assert_eq!(send_id.as_ref(), "send-kick");
            }
            other => panic!("expected AudioModSetSource, got {:?}", other),
        }

        // The Mode row's last button ("Both") — flat index = send_count(1) +
        // the Listen chips (`trigger_source_chips` for the fixture's cell) +
        // 1 (the trailing "Custom" cell) + 2 (Both is the Mode row's 3rd
        // button, index 2). A trigger-gate drawer has no Invert button
        // (placebo on the raw BUG-242 edge) and its Feature/Band matrix is
        // closed by default.
        let ms = &panel.state.mod_state;
        let current = crate::types::AudioFeature::new(
            audio_kind_from_index(ms.audio_kind_idx.get(gi).copied().unwrap_or(0) as usize),
            audio_band_from_index(ms.audio_band_idx.get(gi).copied().unwrap_or(0) as usize),
        );
        let chip_count = trigger_source_chips(current).len();
        let mode_both_btn = button_ids[1 + chip_count + 1 + 2];
        let actions = panel.handle_click(mode_both_btn, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Modulation(ModulationAction::AudioModSetTriggerMode(target, param_id, mode_idx)) => {
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
        let gi = closed.rows.len() - 1;
        closed.audio.rows[gi].active = false; // start disarmed — drawer closed
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
        assert!(panel.row_host.audio_configs[gi].is_some());

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
        assert_eq!(panel.row_host.slider_ids.len(), 2);
        assert!(panel.row_host.slider_ids[0].is_some());
        assert!(panel.row_host.slider_ids[1].is_some());
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

        let value_cell = panel.row_host.slider_ids[0].as_ref().unwrap().value_text;
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
    fn effect_config_with_mappable() -> ParamSurface {
        let mut c = effect_config();
        c.rows[1].mapping.mappable = true;
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
        assert!(panel.row_host.mapping_chevron_ids[0].is_none(), "row 0 not mappable");
        assert!(
            panel.row_host.mapping_chevron_ids[1].is_some(),
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
        assert!(panel.row_host.mapping_chevron_ids.iter().all(|id| id.is_none()));
    }

    #[test]
    fn mapping_chevron_click_emits_open_card_mapping() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.set_context(CardContext::Author);
        panel.configure(&effect_config_with_mappable());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 200.0));

        let chevron = panel.row_host.mapping_chevron_ids[1].expect("row 1 mappable → chevron");
        let actions = panel.handle_click(chevron, &tree);
        assert!(
            matches!(&actions[..], [PanelAction::Root(RootAction::OpenCardMapping(pid))] if pid == "strength"),
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
            c.rows[0].modulation = RowMod { driver_active: driver0, ..Default::default() };
            let mut panel = ParamCardPanel::new();
            panel.set_context(CardContext::Author);
            panel.configure(&c);
            panel.build(&mut tree, Rect::new(0.0, 0.0, 340.0, 300.0));
            let chevron = panel.row_host.mapping_chevron_ids[1].expect("row 1 mappable → chevron");
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

        let chevron = panel.row_host.mapping_chevron_ids[1].expect("row 1 mappable → chevron");
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
    fn generator_config_with_mappable() -> ParamSurface {
        let mut c = effect_config();
        c.kind = ParamCardKind::Generator;
        c.rows[1].mapping.mappable = true;
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
        assert!(panel.row_host.mapping_chevron_ids[0].is_none(), "row 0 not mappable");
        let chevron = panel.row_host.mapping_chevron_ids[1].expect("generator mappable row → chevron");
        let actions = panel.handle_click(chevron, &tree);
        assert!(
            matches!(&actions[..], [PanelAction::Root(RootAction::OpenCardMapping(pid))] if pid == "strength"),
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
        assert!(panel.row_host.mapping_chevron_ids.iter().all(|id| id.is_none()));
    }

    #[test]
    fn handle_click_toggle() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.toggle_btn_id.unwrap(), &tree);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::Params(ParamsAction::EffectToggle(0))));
    }

    #[test]
    fn handle_click_chevron() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.chevron_btn_id.unwrap(), &tree);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::Params(ParamsAction::EffectCollapseToggle(0))));
    }

    #[test]
    fn handle_click_driver_button() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let actions = panel.handle_click(panel.row_host.driver_btn_ids[0].unwrap(), &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Modulation(ModulationAction::DriverToggle(GraphParamTarget::Effect(ei), param_id)) => {
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
        let value_text_id = panel.row_host.slider_ids[0].as_ref().unwrap().value_text;
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
        let fill_id = panel.row_host.slider_ids[0].as_ref().unwrap().fill;
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

        let track = panel.row_host.slider_ids[0].as_ref().unwrap().track;
        let track_rect = tree.get_bounds(panel.row_host.slider_ids[0].as_ref().unwrap().track);
        let mid_x = track_rect.x + track_rect.width * 0.5;

        let down = panel.handle_pointer_down(track, Vec2::new(mid_x, track_rect.y), &tree);
        assert!(
            matches!(down.as_slice(), [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Begin), PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(..))]),
            "begin emits snapshot + first value: {down:?}"
        );
        assert!(panel.is_dragging());

        let quarter_x = track_rect.x + track_rect.width * 0.25;
        let moved = panel.handle_drag(Vec2::new(quarter_x, track_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::Param(target, pid), ScrubPhase::Move(ScrubValue::Scalar(val)))]
                if *target == GraphParamTarget::Effect(0) && pid.as_ref() == "radius" && (*val - 25.0).abs() < 1.0),
            "track emits the live value at the new position: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::Scrub(ValueRef::Param(GraphParamTarget::Effect(0), pid), ScrubPhase::Commit)] if pid.as_ref() == "radius"),
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

        let trim = panel.row_host.trim_ids[0].as_ref().expect("driver trim built");
        let min_bar_id = trim.min_bar_id;

        let down = panel.handle_pointer_down(min_bar_id, Vec2::new(0.0, 0.0), &tree);
        assert!(
            matches!(down.as_slice(), [PanelAction::Scrub(ValueRef::Trim(TrimKind::Driver, GraphParamTarget::Effect(0), pid), ScrubPhase::Begin)] if pid.as_ref() == "radius"),
            "begin emits a trim snapshot: {down:?}"
        );
        assert!(panel.is_dragging());

        let track_rect = tree.get_bounds(panel.row_host.slider_ids[0].as_ref().unwrap().track);
        let new_x = track_rect.x + track_rect.width * 0.4;
        let moved = panel.handle_drag(Vec2::new(new_x, track_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::Trim(TrimKind::Driver, GraphParamTarget::Effect(0), pid), ScrubPhase::Move(..))] if pid.as_ref() == "radius"),
            "track emits the live trim range: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::Scrub(ValueRef::Trim(TrimKind::Driver, GraphParamTarget::Effect(0), pid), ScrubPhase::Commit)] if pid.as_ref() == "radius"),
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

        let track = panel.row_host.slider_ids[0].as_ref().unwrap().track;
        let trim = panel.row_host.trim_ids[0].as_ref().expect("driver trim built");
        let (min_bar, max_bar, fill) = (trim.min_bar_id, trim.max_bar_id, trim.fill_id);

        scroll_shift(&mut tree, 137.0);

        panel.handle_pointer_down(min_bar, Vec2::ZERO, &tree);
        let live = tree.get_bounds(track);
        let moved = panel.handle_drag(Vec2::new(live.x + live.width * 0.3, live.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::Trim(TrimKind::Driver, ..), ScrubPhase::Move(..))]),
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

        let track = panel.row_host.slider_ids[0].as_ref().unwrap().track;
        let target_bar = panel.row_host.target_ids[0].as_ref().expect("envelope target built").target_bar_id;

        scroll_shift(&mut tree, 137.0);

        panel.handle_pointer_down(target_bar, Vec2::ZERO, &tree);
        let live = tree.get_bounds(track);
        let moved = panel.handle_drag(Vec2::new(live.x + live.width * 0.5, live.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::EnvelopeTarget(..), ScrubPhase::Move(..))]),
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

        let target = panel.row_host.target_ids[0].as_ref().expect("envelope target built");
        let target_bar_id = target.target_bar_id;

        let down = panel.handle_pointer_down(target_bar_id, Vec2::new(0.0, 0.0), &tree);
        assert!(
            matches!(down.as_slice(), [PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Effect(0), pid), ScrubPhase::Begin)] if pid.as_ref() == "radius"),
            "begin emits a target snapshot: {down:?}"
        );
        assert!(panel.is_dragging());

        let track_rect = tree.get_bounds(panel.row_host.slider_ids[0].as_ref().unwrap().track);
        let new_x = track_rect.x + track_rect.width * 0.3;
        let moved = panel.handle_drag(Vec2::new(new_x, track_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Effect(0), pid), ScrubPhase::Move(ScrubValue::Scalar(norm)))] if pid.as_ref() == "radius" && (*norm - 0.3).abs() < 0.05),
            "track emits the live target norm: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::Scrub(ValueRef::EnvelopeTarget(GraphParamTarget::Effect(0), pid), ScrubPhase::Commit)] if pid.as_ref() == "radius"),
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

        let cfg = panel.row_host.envelope_config_ids[0].as_ref().expect("envelope config built");
        let decay_track = cfg.decay_slider.track;
        let decay_rect = tree.get_bounds(cfg.decay_slider.track);

        let down = panel.handle_pointer_down(decay_track, Vec2::new(decay_rect.x, decay_rect.y), &tree);
        assert!(
            matches!(
                down.as_slice(),
                [PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Effect(0), pid1), ScrubPhase::Begin), PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Effect(0), pid2), ScrubPhase::Move(..))]
                if pid1.as_ref() == "radius" && pid2.as_ref() == "radius"
            ),
            "begin emits snapshot + first decay value: {down:?}"
        );
        assert!(panel.is_dragging());

        let new_x = decay_rect.x + decay_rect.width * 0.6;
        let moved = panel.handle_drag(Vec2::new(new_x, decay_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Effect(0), pid), ScrubPhase::Move(..))] if pid.as_ref() == "radius"),
            "track emits the live decay value: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::Scrub(ValueRef::EnvDecay(GraphParamTarget::Effect(0), pid), ScrubPhase::Commit)] if pid.as_ref() == "radius"),
            "end emits exactly one decay commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
    }

    /// Fixture with param 0's audio mod armed and Continuous — exercises the
    /// shaping sliders (Sensitivity/Attack/Release, `DrawerIds.sliders[0..3]`).
    fn effect_config_with_audio_shape_armed() -> ParamSurface {
        let mut c = effect_config();
        let n = c.rows.len();
        c.audio.send_labels = vec!["Kick".into()];
        c.audio.send_ids = vec![manifold_foundation::AudioSendId::new("send-kick")];
        c.audio.rows = vec![AudioRowState::default(); n];
        c.audio.rows[0].active = true;
        c.audio.rows[0].send_id = Some(manifold_foundation::AudioSendId::new("send-kick"));
        c
    }

    #[test]
    fn pinning_audio_shape_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_audio_shape_armed());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (dids, _) = panel.row_host.audio_configs[0].as_ref().expect("audio drawer built");
        let sens_slider = dids.sliders[0]; // Sensitivity — the first shaping slider
        let sens_track = sens_slider.track;
        let sens_rect = tree.get_bounds(sens_track);

        let down = panel.handle_pointer_down(sens_track, Vec2::new(sens_rect.x, sens_rect.y), &tree);
        assert!(
            matches!(
                down.as_slice(),
                [PanelAction::Scrub(ValueRef::AudioModShape(GraphParamTarget::Effect(0), pid1, AudioShapeParam::Sensitivity), ScrubPhase::Begin), PanelAction::Scrub(ValueRef::AudioModShape(GraphParamTarget::Effect(0), pid2, AudioShapeParam::Sensitivity), ScrubPhase::Move(..))]
                if pid1.as_ref() == "radius" && pid2.as_ref() == "radius"
            ),
            "begin emits snapshot + first shape value: {down:?}"
        );
        assert!(panel.is_dragging());

        let new_x = sens_rect.x + sens_rect.width * 0.7;
        let moved = panel.handle_drag(Vec2::new(new_x, sens_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::AudioModShape(GraphParamTarget::Effect(0), pid, AudioShapeParam::Sensitivity), ScrubPhase::Move(..))] if pid.as_ref() == "radius"),
            "track emits the live shape value: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::Scrub(ValueRef::AudioModShape(GraphParamTarget::Effect(0), pid, _), ScrubPhase::Commit)] if pid.as_ref() == "radius"),
            "end emits exactly one shape commit: {ended:?}"
        );
        assert!(!panel.is_dragging());
    }

    #[test]
    fn pinning_step_amount_drag_begin_track_end() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        let mut cfg = effect_config_with_audio_shape_armed();
        cfg.audio.rows[0].action_idx = 1; // Step — the 4th drawer slider appears
        panel.configure(&cfg);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));

        let (dids, _) = panel.row_host.audio_configs[0].as_ref().expect("audio drawer built");
        let step_slider = *dids.sliders.get(3).expect("Step slider built while Action=Step");
        let step_track = step_slider.track;
        let step_rect = tree.get_bounds(step_track);

        let down = panel.handle_pointer_down(step_track, Vec2::new(step_rect.x, step_rect.y), &tree);
        assert!(
            matches!(
                down.as_slice(),
                [PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Effect(0), pid1), ScrubPhase::Begin), PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Effect(0), pid2), ScrubPhase::Move(..))]
                if pid1.as_ref() == "radius" && pid2.as_ref() == "radius"
            ),
            "begin emits snapshot + first step value: {down:?}"
        );
        assert!(panel.is_dragging());

        let new_x = step_rect.x + step_rect.width * 0.8;
        let moved = panel.handle_drag(Vec2::new(new_x, step_rect.y), &mut tree);
        assert!(
            matches!(moved.as_slice(), [PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Effect(0), pid), ScrubPhase::Move(..))] if pid.as_ref() == "radius"),
            "track emits the live step value: {moved:?}"
        );

        let ended = panel.handle_drag_end(&mut tree);
        assert!(
            matches!(ended.as_slice(), [PanelAction::Scrub(ValueRef::AudioModStepAmount(GraphParamTarget::Effect(0), pid), ScrubPhase::Commit)] if pid.as_ref() == "radius"),
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

        assert!(panel.row_host.driver_config_ids[0].is_some());
        assert!(panel.row_host.trim_ids[0].is_some());
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
        armed.rows[0].modulation.driver_active = true;
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
        assert!(panel.row_host.driver_config_ids[0].is_some(), "driver config built (unclipped) when settled");
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
        armed.rows[0].modulation.driver_active = true;

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
        assert!(panel.row_host.target_ids[0].is_some());
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
            panel.row_host.mod_tab_ids[0].len(),
            2,
            "tab strip shows both active configs"
        );
        assert!(
            panel.row_host.driver_config_ids[0].is_some(),
            "the shown config (driver) is built"
        );
        assert!(
            panel.row_host.envelope_config_ids[0].is_none(),
            "the hidden config is not built (no stacking)"
        );
        // Track overlays still show for every armed mod regardless of the tab.
        assert!(panel.row_host.trim_ids[0].is_some(), "driver trim stays on the track");
        assert!(
            panel.row_host.target_ids[0].is_some(),
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

        let (env_tab_id, _) = panel.row_host.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Envelope)
            .copied()
            .expect("envelope tab present");
        let actions = panel.handle_click(env_tab_id, &tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Params(ParamsAction::ModConfigTabChanged)]));
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

        let (driver_id, _) = panel.row_host.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Driver)
            .copied()
            .expect("driver tab present");
        let driver_x = tree.get_bounds(driver_id).x;
        // First build: the ink was "never positioned" (fresh AnimF32), so it
        // snapped straight to the Driver tab's x rather than sliding in.
        assert_eq!(panel.mod_tab_ink[0].value(), driver_x);
        assert!(!panel.mod_tab_ink[0].is_animating());

        let (env_id, _) = panel.row_host.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Envelope)
            .copied()
            .expect("envelope tab present");
        let env_x = tree.get_bounds(env_id).x;
        assert_ne!(env_x, driver_x, "the two tabs occupy different x positions");

        panel.handle_click(env_id, &tree);
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

        let (env_tab_id, _) = panel.row_host.mod_tab_ids[0]
            .iter()
            .find(|(_, t)| *t == ModTab::Envelope)
            .copied()
            .expect("envelope tab present");
        panel.handle_click(env_tab_id, &tree);
        assert_eq!(panel.mod_active_tab[0], ModTab::Envelope);

        // Re-sync (same effect) → configure must not clobber the tab choice.
        panel.configure(&effect_config());
        assert_eq!(
            panel.mod_active_tab[0],
            ModTab::Envelope,
            "configure reset the tab — the snap-back bug would be back"
        );

        // And the rebuilt drawer shows the envelope config, not the driver.
        // (Clear first: live rebuilds truncate the card's region, so two live
        // copies never coexist — identity-keyed roots correctly assert on a
        // no-clear double build.)
        panel.state.mod_state.driver_expanded[0] = true;
        panel.state.mod_state.envelope_expanded[0] = true;
        tree.clear();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
        assert!(panel.row_host.envelope_config_ids[0].is_some());
        assert!(panel.row_host.driver_config_ids[0].is_none());
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
        let label_x = |cfg: &ParamSurface| -> f32 {
            let mut tree = UITree::new();
            let mut panel = ParamCardPanel::new();
            panel.configure(cfg);
            panel.set_collapsed(false);
            panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 400.0));
            panel.row_host
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

    fn gen_config() -> ParamSurface {
        ParamSurface {
            kind: ParamCardKind::Generator,
            title: "Plasma".into(),
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
            has_graph_mod: false,
            layer_id: None,
            rows: vec![
                ParamRow {
                    id: std::borrow::Cow::Borrowed("speed"),
                    spec: RowSpec {
                        name: "Speed".into(),
                        min: 0.0,
                        max: 10.0,
                        default: 1.0,
                        whole_numbers: false,
                        is_angle: false,
                        is_toggle: false,
                        is_trigger: false,
                        is_trigger_gate: false,
                        value_labels: None,
                        section: None,
                    },
                    value: RowValue { base: 1.0, effective: 1.0, exposed: true, driven: false },
                    modulation: RowMod::default(),
                    mapping: RowMapping {
                        osc_address: None,
                        ableton_display: None,
                        ableton_range: None,
                        mappable: false,
                    },
                },
                ParamRow {
                    id: std::borrow::Cow::Borrowed("invert"),
                    spec: RowSpec {
                        name: "Invert".into(),
                        min: 0.0,
                        max: 1.0,
                        default: 0.0,
                        whole_numbers: false,
                        is_angle: false,
                        is_toggle: true,
                        is_trigger: false,
                        is_trigger_gate: false,
                        value_labels: None,
                        section: None,
                    },
                    value: RowValue { base: 0.0, effective: 0.0, exposed: true, driven: false },
                    modulation: RowMod::default(),
                    mapping: RowMapping {
                        osc_address: None,
                        ableton_display: None,
                        ableton_range: None,
                        mappable: false,
                    },
                },
                ParamRow {
                    id: std::borrow::Cow::Borrowed("scale"),
                    spec: RowSpec {
                        name: "Scale".into(),
                        min: 0.1,
                        max: 5.0,
                        default: 1.0,
                        whole_numbers: false,
                        is_angle: false,
                        is_toggle: false,
                        is_trigger: false,
                        is_trigger_gate: false,
                        value_labels: None,
                        section: None,
                    },
                    value: RowValue { base: 1.0, effective: 1.0, exposed: true, driven: false },
                    modulation: RowMod::default(),
                    mapping: RowMapping {
                        osc_address: None,
                        ableton_display: None,
                        ableton_range: None,
                        mappable: false,
                    },
                },
            ],
            string_params: vec![],
            audio: Default::default(),
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
        assert!(panel.row_host.slider_ids[0].is_some()); // Speed = slider
        assert!(panel.row_host.toggle_ids[1].is_some()); // Invert = toggle
        assert!(panel.row_host.slider_ids[2].is_some()); // Scale = slider
        assert!(panel.node_count > 0);
    }

    #[test]
    fn handle_click_gen_type() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        // Clicking the Change button opens the type picker
        let actions = panel.handle_click(panel.change_btn_id.unwrap(), &tree);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::Params(ParamsAction::GenTypeClicked(_))));

        // Clicking the name label selects the card
        let actions = panel.handle_click(panel.name_label_id.unwrap(), &tree);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::Params(ParamsAction::GenCardClicked)));
    }

    /// D5 card sections (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): Speed +
    /// Invert sectioned under "Leaf" (a contiguous run), Scale unsectioned.
    fn gen_config_with_sections() -> ParamSurface {
        let mut c = gen_config();
        c.rows[0].spec.section = Some("Leaf".to_string());
        c.rows[1].spec.section = Some("Leaf".to_string());
        c.rows[2].spec.section = None;
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

        assert_eq!(panel.row_host.section_header_ids.len(), 1, "exactly one header — the Leaf run");
        assert_eq!(panel.row_host.section_header_ids[0].1, "Leaf");
        // Every row still builds (unfolded by default) — Speed/Invert/Scale
        // widgets all present, same as the no-sections case.
        assert!(panel.row_host.slider_ids[0].is_some());
        assert!(panel.row_host.toggle_ids[1].is_some());
        assert!(panel.row_host.slider_ids[2].is_some());
    }

    #[test]
    fn clicking_a_section_header_folds_it_and_a_rebuild_skips_its_rows() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config_with_sections());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let header_id = panel.row_host.section_header_ids[0].0;
        let actions = panel.handle_click(header_id, &tree);
        assert!(
            matches!(actions.as_slice(), [PanelAction::Params(ParamsAction::SectionFoldToggled)]),
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
        assert_eq!(panel.row_host.section_header_ids.len(), 1, "the header itself still draws while folded");
        assert!(panel.row_host.slider_ids[0].is_none(), "Speed's row was skipped (folded)");
        assert!(panel.row_host.toggle_ids[1].is_none(), "Invert's row was skipped (folded)");
        assert!(panel.row_host.slider_ids[2].is_some(), "Scale (outside the folded run) still builds");

        // Click again — unfolds.
        let actions = panel.handle_click(header_id, &tree);
        assert!(matches!(actions.as_slice(), [PanelAction::Params(ParamsAction::SectionFoldToggled)]));
        assert_eq!(panel.section_folded.get("Leaf"), Some(&false));
        let mut tree3 = UITree::new();
        panel.build(&mut tree3, Rect::new(0.0, 0.0, 280.0, 300.0));
        assert!(panel.row_host.slider_ids[0].is_some(), "Speed's row is back once unfolded");
    }

    #[test]
    fn handle_click_toggle_param() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&gen_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let button_id = panel.row_host.toggle_ids[1].as_ref().unwrap().button_id;
        let actions = panel.handle_click(button_id, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Params(ParamsAction::ParamToggle(target, param_id)) => {
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
            &panel.rows[0],
            &panel.state.mod_state,
            0,
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

        let track = panel.row_host.slider_ids[0].as_ref().unwrap().track;
        match reg.resolve(&tree, Some(track), crate::intent::Gesture::RightClick) {
            Some(PanelAction::Root(RootAction::SliderReset { changed, .. })) => {
                assert!(matches!(*changed, PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(ScrubValue::Scalar(v))) if (v - 10.0).abs() < f32::EPSILON));
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

        let dids = &panel.row_host.audio_configs[0].as_ref().expect("audio drawer built").0;
        assert_eq!(dids.sliders.len(), 3, "Amount/Attack/Release");

        let expected = [
            (AudioShapeParam::Sensitivity, AUDIO_SENS_DEFAULT),
            (AudioShapeParam::Attack, AUDIO_ATTACK_DEFAULT_MS),
            (AudioShapeParam::Release, AUDIO_RELEASE_DEFAULT_MS),
        ];
        for (si, (which, default)) in expected.into_iter().enumerate() {
            let track = dids.sliders[si].track;
            match reg.resolve(&tree, Some(track), crate::intent::Gesture::RightClick) {
                Some(PanelAction::Root(RootAction::SliderReset { changed, .. })) => match *changed {
                    PanelAction::Scrub(ValueRef::AudioModShape(_, _, got_which), ScrubPhase::Move(ScrubValue::Scalar(v))) => {
                        assert_eq!(got_which, which);
                        assert!((v - default).abs() < f32::EPSILON, "slider {si}: {v} != {default}");
                    }
                    other => panic!("slider {si}: expected AudioModShape scrub move, got {other:?}"),
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
            Some(PanelAction::Root(RootAction::SliderReset { .. })) => {}
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

        let gi = panel.rows.len() - 1;
        assert!(panel.row_host.slider_ids[gi].is_none(), "trigger-gate row has no main slider");

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let dids = &panel.row_host.audio_configs[gi].as_ref().expect("audio drawer armed in fixture").0;
        // Param-drawer unification (2026-07-19): a trigger-gate target fires
        // on the raw BUG-242 edge, so Attack/Release are placebo there and
        // the drawer builds only Sensitivity.
        assert_eq!(dids.sliders.len(), 1, "Sensitivity only — Attack/Release dropped on trigger-gate");
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

        let track = panel.row_host.slider_ids[0].as_ref().unwrap().track;
        assert_track_resets(&reg, &tree, track);
    }

    // ── P2 dispatch-family coverage (`docs/WIDGET_TREE_DESIGN.md` §6/P2) —
    // roles the pre-existing suite above didn't already regression-pin
    // (DriverBtn/AudioBtn/ToggleBtn/MappingChevron/SectionHeader/ModTab were
    // covered before this lane and stayed green through the RowIndex swap;
    // these close the remaining gaps: EnvelopeBtn, RowCatcher, Slider's
    // label/value-cell sub-elements, and the toggle-row Label role). ──

    fn effect_config_with_osc_and_enum() -> ParamSurface {
        let mut c = effect_config();
        c.rows[0].mapping.osc_address = Some("/fx/0/radius".into());
        c.rows[1].spec.value_labels = Some(vec!["Low".into(), "High".into()]);
        c
    }

    #[test]
    fn row_dispatch_envelope_btn_arms_envelope() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let btn = panel.row_host.envelope_btn_ids[0].expect("row 0 supports envelopes");
        let actions = panel.handle_click(btn, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Modulation(ModulationAction::EnvelopeToggle(GraphParamTarget::Effect(ei), param_id)) => {
                assert_eq!(*ei, 0);
                assert_eq!(param_id.as_ref(), "radius");
            }
            other => panic!("expected EnvelopeToggle, got {other:?}"),
        }
    }

    #[test]
    fn row_dispatch_row_catcher_click_is_a_left_click_no_op() {
        // RowCatcher carries only the RIGHT-click param-menu contract
        // (`register_intents`) — a plain left click, same as the old
        // gauntlet (which never checked `row_catcher_ids` in `handle_click`),
        // emits nothing.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let catcher = panel.row_host.row_catcher_ids[0].expect("row 0 built a catcher");
        let actions = panel.handle_click(catcher, &tree);
        assert!(actions.is_empty(), "left click on the row catcher must be a no-op, got {actions:?}");
    }

    #[test]
    fn row_dispatch_slider_label_click_copies_osc_address() {
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_osc_and_enum());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let label = panel.row_host.slider_ids[0].as_ref().unwrap().label.expect("row 0 has a label");
        let actions = panel.handle_click(label, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Root(RootAction::CopyOscAddress(addr)) => assert_eq!(addr, "/fx/0/radius"),
            other => panic!("expected CopyOscAddress, got {other:?}"),
        }
    }

    #[test]
    fn row_dispatch_slider_label_click_with_no_osc_address_is_a_no_op() {
        // Row 1 ("strength") carries no OSC address in this fixture — the
        // label click must fall through empty, matching `RowRole::Slider`'s
        // `osc_addresses[row].is_some()` guard.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_osc_and_enum());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let label = panel.row_host.slider_ids[1].as_ref().unwrap().label.expect("row 1 has a label");
        let actions = panel.handle_click(label, &tree);
        assert!(actions.is_empty(), "no OSC address on this row → no-op, got {actions:?}");
    }

    #[test]
    fn row_dispatch_enum_value_cell_click_resolves() {
        // Row 1 ("strength") carries `value_labels` in this fixture — a
        // click on the value cell resolves through `enum_value_cell_action`
        // (BUG-250), not the double-click type-in path.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config_with_osc_and_enum());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let value_cell = panel.row_host.slider_ids[1].as_ref().unwrap().value_text;
        let actions = panel.handle_click(value_cell, &tree);
        assert!(!actions.is_empty(), "an enum row's value-cell click must resolve to an action");
    }

    #[test]
    fn row_identity_survives_an_earlier_row_arming_a_drawer() {
        // D4 (`docs/WIDGET_TREE_DESIGN.md`): arming a modulator on row 0
        // inserts its config-drawer nodes as SIBLINGS ahead of row 1's own
        // controls (build_effect_sliders parents every row flat to the
        // card's inner-bg) — before P2 this renumbered row 1's auto-salted
        // driver button; keyed-by-`ParamId` identity must not move.
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        panel.configure(&effect_config());
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));
        let row1_driver_btn_before = tree.widget_of(panel.row_host.driver_btn_ids[1].unwrap());
        let row1_slider_track_before = tree.widget_of(panel.row_host.slider_ids[1].as_ref().unwrap().track);

        tree.clear();
        panel.state.mod_state.driver_expanded[0] = true;
        panel.mod_active_tab[0] = ModTab::Driver;
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));
        let row1_driver_btn_after = tree.widget_of(panel.row_host.driver_btn_ids[1].unwrap());
        let row1_slider_track_after = tree.widget_of(panel.row_host.slider_ids[1].as_ref().unwrap().track);

        assert_eq!(
            row1_driver_btn_before, row1_driver_btn_after,
            "row 1's driver button must keep its WidgetId once row 0 grows a drawer ahead of it"
        );
        assert_eq!(
            row1_slider_track_before, row1_slider_track_after,
            "row 1's slider track must keep its WidgetId once row 0 grows a drawer ahead of it"
        );
    }

    #[test]
    fn card_identity_survives_effect_chain_reorder() {
        // D4's card-root half (fork-review BLOCKER-1): cards are siblings
        // under the inspector column, so the card ROOT is keyed by its
        // `EffectId` — reordering the chain must not renumber a later
        // card's row widgets. Two cards build as root siblings here, then
        // swap build order (the reorder), and card B's row-0 controls must
        // keep their WidgetIds.
        let mut cfg_a = effect_config();
        let mut cfg_b = effect_config();
        cfg_a.effect_id = manifold_foundation::EffectId::from("fx-card-a");
        cfg_b.effect_id = manifold_foundation::EffectId::from("fx-card-b");
        cfg_a.effect_index = 0;
        cfg_b.effect_index = 1;

        let mut tree = UITree::new();
        let mut panel_a = ParamCardPanel::new();
        let mut panel_b = ParamCardPanel::new();
        panel_a.configure(&cfg_a);
        panel_b.configure(&cfg_b);
        panel_a.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));
        panel_b.build(&mut tree, Rect::new(0.0, 310.0, 280.0, 300.0));
        let b_driver_before = tree.widget_of(panel_b.row_host.driver_btn_ids[0].unwrap());
        let b_track_before = tree.widget_of(panel_b.row_host.slider_ids[0].as_ref().unwrap().track);

        // The reorder: B now builds FIRST (sibling order swapped).
        tree.clear();
        panel_b.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));
        panel_a.build(&mut tree, Rect::new(0.0, 310.0, 280.0, 300.0));
        let b_driver_after = tree.widget_of(panel_b.row_host.driver_btn_ids[0].unwrap());
        let b_track_after = tree.widget_of(panel_b.row_host.slider_ids[0].as_ref().unwrap().track);

        assert_eq!(
            b_driver_before, b_driver_after,
            "card B's driver button must keep its WidgetId across a chain reorder"
        );
        assert_eq!(
            b_track_before, b_track_after,
            "card B's slider track must keep its WidgetId across a chain reorder"
        );
    }

    #[test]
    fn row_dispatch_toggle_row_label_click_copies_osc_address() {
        // The `Label` role — distinct from `Slider`'s label sub-element
        // (toggle/trigger rows carry no slider bundle to bundle it under).
        let mut tree = UITree::new();
        let mut panel = ParamCardPanel::new();
        let mut cfg = effect_config_with_toggle_and_trigger();
        let toggle_row = cfg.rows.len() - 2; // "invert", the is_toggle row
        cfg.rows[toggle_row].mapping.osc_address = Some("/fx/0/invert".into());
        panel.configure(&cfg);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 200.0));

        let label = panel.row_host.toggle_ids[toggle_row]
            .as_ref()
            .expect("toggle row built")
            .label_id
            .expect("toggle row always builds a label");
        let actions = panel.handle_click(label, &tree);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::Root(RootAction::CopyOscAddress(addr)) => assert_eq!(addr, "/fx/0/invert"),
            other => panic!("expected CopyOscAddress, got {other:?}"),
        }
    }
}
