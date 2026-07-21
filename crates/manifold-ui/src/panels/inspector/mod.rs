use crate::{ParamsAction, RootAction, TransportAction};
use super::clip_chrome::ClipChromePanel;
use super::layer_chrome::LayerChromePanel;
use super::audio_trigger_section::AudioTriggerSection;
use super::macros_panel::MacrosPanel;
use super::master_chrome::MasterChromePanel;
use super::param_card::{CardContext, ParamCardPanel};
use crate::param_surface::ParamSurface;
use super::{InspectorTab, Panel, PanelAction};
use crate::chrome::{self, Pad, View};
use crate::color;
use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::{UITree, ZTier};
use manifold_foundation::EffectId;
use manifold_foundation::LayerId;
use std::collections::HashSet;
use std::time::Instant;

mod card_drag;
mod render;
mod routing;


// ── Layout constants ────────────────────────────────────────────
// §14.5 E — the container owns the inter-card gap (one owner): the canonical
// `SPACE_M`, paired with `param_card::CARD_BOTTOM_MARGIN` → 0 (was 6 + 6 = 12).
const SECTION_GAP: f32 = color::SPACE_M;
const SECTION_CARD_RADIUS: f32 = color::CARD_RADIUS; // §14.2 rule 6: section cards = CARD_RADIUS
const SECTION_CARD_PAD: f32 = 6.0;
const SECTION_CARD_BG: Color32 = Color32::new(22, 22, 23, 255);
const SECTION_CARD_BORDER: Color32 = Color32::new(50, 50, 54, 255);
const COLUMN_PAD: f32 = 4.0;
const SECTION_INSET: f32 = 4.0; // horizontal padding inside section cards

const SCROLLBAR_STYLE: ScrollbarStyle = ScrollbarStyle {
    track_color: color::SCROLLBAR_TRACK_C32,
    thumb_color: color::SCROLLBAR_THUMB_C32,
    thumb_hover_color: color::SCROLLBAR_THUMB_HOVER_C32,
    corner_radius: color::SMALL_RADIUS,
};

const ADD_EFFECT_BTN_H: f32 = 26.0;

// ── Tab strip ───────────────────────────────────────────────────
const TAB_STRIP_HEIGHT: f32 = 24.0;
const TAB_GAP: f32 = 2.0;
/// Width of the collapse-all / expand-all control reserved at the right edge of
/// the tab strip. The tabs lay out in the remaining width.
const COLLAPSE_ALL_W: f32 = 60.0;
/// §6b — width of the "hide mod settings" (compact) gear toggle, left of the
/// collapse-all control.
const COMPACT_TOGGLE_W: f32 = 26.0;
const TAB_FONT_SIZE: u16 = 12;

/// Key for recovering the materialised "+ Add Effect" button's node id (the same
/// key is reused per column — each [`chrome::materialize`] call returns only its
/// own button).
const KEY_ADD_EFFECT_BTN: u64 = 95_001;

/// A section-card background — the outer 1px border panel + the inset inner fill,
/// as a typed Chrome view. Materialised into the section's rect each frame;
/// byte-identical to the hand-rolled border+bg `add_panel` pair it replaces.
fn section_card_view() -> View {
    View::stack()
        .fill()
        .style(UIStyle {
            bg_color: SECTION_CARD_BORDER,
            corner_radius: SECTION_CARD_RADIUS,
            ..UIStyle::default()
        })
        .pad(Pad::all(1.0))
        .child(View::panel().fill().style(UIStyle {
            bg_color: SECTION_CARD_BG,
            corner_radius: SECTION_CARD_RADIUS - 1.0,
            ..UIStyle::default()
        }))
}

/// The "+ Add Effect" button as a typed Chrome view, keyed so the materialised
/// node id is recoverable. Inert — the click routes through `handle_click`
/// against the stored id, not a Chrome intent.
fn add_effect_button_view() -> View {
    View::button("+ Add Effect")
        .fill()
        // The neutral kit button — the "+" carries the add affordance, no bespoke
        // blue tint (one control look across the app).
        .style(chrome::components::button_secondary_style())
        .inert()
        .key(KEY_ADD_EFFECT_BTN)
}

// ── Effect card drag-reorder constants (Unity EffectsListBitmapPanel) ──
const DRAG_GHOST_H: f32 = 24.0;
const DRAG_GHOST_FONT_SIZE: u16 = color::FONT_BODY;
const DRAG_GHOST_BG: Color32 = Color32::new(60, 80, 120, 200);
const DRAG_GHOST_TEXT: Color32 = Color32::new(220, 220, 230, 255);
const DRAG_INDICATOR_H: f32 = 2.0;
const DRAG_INDICATOR_INSET: f32 = 4.0;
const DRAG_INDICATOR_COLOR: Color32 = color::ACCENT_BLUE_C32;

// ── Pressed target (for drag routing) ───────────────────────────

#[derive(Debug, Clone, Copy)]
enum PressedTarget {
    Macros,
    AudioTriggers,
    MasterChrome,
    LayerChrome,
    ClipChrome,
    MasterEffect(usize),
    LayerEffect(usize),
    GenParam,
    Scrollbar,
}

// ── InspectorCompositePanel ─────────────────────────────────────

/// Composite inspector panel that stacks chrome + effect cards in a
/// scrollable column.
///
/// Layout (top to bottom):
///   Master chrome → Master effect cards
///   Layer chrome  → Layer effect cards
///   Clip chrome   → Gen params → Clip effect cards
///
/// The app layer routes drag events directly via `handle_drag` /
/// `handle_drag_end` (which need `&mut UITree` for slider feedback).
/// All other events go through the `Panel::handle_event` trait method.
///
/// Scrolling: the app layer calls `handle_scroll(delta)` on mouse wheel
/// events within the inspector viewport, then triggers a rebuild.
pub struct InspectorCompositePanel {
    // Sub-panels
    macros_panel: MacrosPanel,
    /// P3b: layer-owned clip-trigger authoring (AUDIO_SETUP_DOCK_AND_TRIGGER_
    /// UNIFICATION_DESIGN.md). Pinned at the TOP of the layer column's
    /// content, above `gen_params`/the layer scope's effect cards — see
    /// `build_in_rect`.
    audio_trigger_section: AudioTriggerSection,
    master_chrome: MasterChromePanel,
    layer_chrome: LayerChromePanel,
    clip_chrome: ClipChromePanel,
    /// one storage for both scopes, indexed by
    /// [`Self::scope_idx`] (`SCOPE_MASTER` / `SCOPE_LAYER`) instead of two
    /// parallel `Vec<ParamCardPanel>` fields. `Layer`/`Group`/`Clip` all
    /// canonicalize to `SCOPE_LAYER` — every former per-tab touchpoint now
    /// routes through [`Self::cards_for_tab`] / [`Self::cards_for_tab_mut`]
    /// (or, when it genuinely needs both scopes at once, `self.effects[..]`
    /// directly) instead of duplicating a match arm per touchpoint.
    effects: [Vec<ParamCardPanel>; 2],
    gen_params: Option<ParamCardPanel>,
    /// D17 "delete collapse" (exit-state pattern, `anim.rs`'s doc comment) —
    /// cards `reconcile_cards` no longer finds a config for, kept alive here
    /// so they keep collapsing/fading instead of vanishing the instant the
    /// model drops them. Drawn after the live cards in the same column
    /// (append-only — a dying card doesn't preserve its old list position,
    /// a deliberate simplification: reordering the live list around a
    /// disappearing card would need the FlipList displacement machinery for
    /// no visible benefit, since it's shrinking to nothing anyway). Pruned in
    /// `update()` once `ParamCardPanel::is_delete_finished` is true.
    master_dying: Vec<ParamCardPanel>,
    layer_dying: Vec<ParamCardPanel>,
    /// The layer whose effects `effects[SCOPE_LAYER]` currently holds. When
    /// `configure_layer_effects` is called for a DIFFERENT scope (a different
    /// selected layer, or none), that's navigation — not an edit of the
    /// current chain — so the old cards are dropped instantly rather than
    /// routed through the `layer_dying` delete-collapse (their effects weren't
    /// deleted, just navigated away from). Only a same-scope reconcile keeps
    /// the exit animation. Twin of `configure_gen_params`, which already keys
    /// panel reuse on the layer id.
    layer_scope_id: Option<LayerId>,

    /// Chrome context applied to every card this panel owns (Perform on the
    /// main window's inspector, Author on the graph-editor window's — set
    /// once by the host at construction, per `ParamCardPanel::set_context`'s
    /// doc comment). Stored here (not just pushed to existing cards) so a
    /// freshly-created card picks it up too — `reconcile_cards` and
    /// `configure_gen_params` apply it to every card they build.
    card_context: CardContext,

    // ── Tabs ──
    /// The single scope currently shown. Drives the section-visibility bools
    /// below (only the active scope renders). Mirrors the timeline selection;
    /// set by the app via `configure_tabs`. See docs/UI_LAYOUT_DESIGN.md.
    active_tab: InspectorTab,
    /// The tab rungs available for the current selection, in display order
    /// (local→global). Only the rungs that exist are shown.
    available_tabs: Vec<InspectorTab>,
    /// Node id → tab, for routing tab-strip clicks.
    tab_node_ids: Vec<(NodeId, InspectorTab)>,
    /// The collapse-all / expand-all control at the right of the tab strip.
    collapse_all_btn_id: Option<NodeId>,
    /// §6b — the "hide mod settings" (compact) toggle, left of collapse-all.
    compact_toggle_btn_id: Option<NodeId>,
    /// §6b — global compact mode: hide every card's modulation config drawers
    /// (mods stay armed). UI-only, propagated to all cards each build.
    mods_compact: bool,

    // Section visibility is derived from `active_tab` (the single source of
    // truth) via the master_visible() / layer_visible() / clip_visible()
    // accessors — no separate cached booleans.

    // Add Effect button node IDs
    add_master_effect_btn: Option<NodeId>,
    add_layer_effect_btn: Option<NodeId>,

    // Scroll state — two independent columns via ScrollContainer
    master_scroll: ScrollContainer,
    layer_scroll: ScrollContainer,
    viewport_rect: Rect,
    /// X boundary between master (left) and layer (right) columns.
    column_split_x: f32,
    /// Y where columns start (below macros panel).
    columns_y: f32,
    /// Height available for columns (viewport height minus macros).
    columns_height: f32,
    dragging_scrollbar: bool,
    dragging_scrollbar_master: bool,
    /// Set whenever a scroll (wheel or scrollbar drag) offset the content nodes
    /// in place this frame. The app drains it with `take_scrolled_in_place` and
    /// invalidates only the inspector's atlas slot — one signal for both scroll
    /// inputs, so neither has to know about the cache.
    scrolled_in_place: bool,

    // Event routing
    pressed_target: Option<PressedTarget>,
    /// Remembers which inspector tab (Master/Layer/Clip) the last effect
    /// interaction targeted. Survives across drag_end so dispatch can
    /// route effect actions to the correct data location.
    last_effect_tab: InspectorTab,

    // Background
    bg_panel_id: Option<NodeId>,

    // ── Effect selection state (Unity EffectSelectionManager — per tab) ──
    selected_master_ids: HashSet<EffectId>,
    selected_layer_ids: HashSet<EffectId>,
    last_clicked_master: Option<EffectId>,
    last_clicked_layer: Option<EffectId>,

    // ── Effect card drag-reorder state (Unity EffectsListBitmapPanel) ──
    card_drag_active: bool,
    card_drag_tab: InspectorTab,
    card_drag_source_index: usize, // index within the tab's effect cards vec
    card_drag_effect_index: usize, // effect_index in the flat effects list
    card_drag_target_index: usize, // current drop target index
    card_drag_ghost_id: Option<NodeId>,
    card_drag_indicator_id: Option<NodeId>,
    card_drag_label: String,
    /// The `Ghost`-tier region (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1–D3)
    /// wrapping the ghost + indicator, minted fresh in
    /// [`try_begin_card_drag`](Self::try_begin_card_drag) each time a drag
    /// starts. Its root sits BEFORE `card_drag_ghost_id` in the tree, so
    /// [`card_drag_first_node`](Self::card_drag_first_node) reports this
    /// index (not the ghost label's own) — the render pass's
    /// `render_tree_range(start, usize::MAX)` walks registered regions, not
    /// a raw root scan, and would miss the region entirely otherwise.
    card_drag_region_root: Option<NodeId>,

    // Cache tracking
    cache_first_node: usize,
    cache_node_count: usize,

    // ── P1 drawer motion ──
    /// True while any card's drawer-height tween is in flight (or settled this
    /// frame — see `update`). The app polls `drawer_anim_active()` after
    /// `ui_root.update()` and forces a rebuild while it's true, so the
    /// interpolated height re-lays-out and content below reflows.
    drawer_anim_active: bool,
    /// Whether any tween was advancing last frame — keeps `drawer_anim_active`
    /// true for one extra frame after the last one settles, so the final (target)
    /// value gets one build to render (the settling tick returns false, but its
    /// new value still needs a rebuild to reach the screen).
    drawer_anim_prev: bool,
    /// Wall-clock anchor for this frame's tween `dt_ms` — the inspector has no
    /// frame timer, so it measures its own delta (same pattern as the layer
    /// header's mute-chip motion). `None` until the first `update()` call, so
    /// that call's dt is always exactly 0 instead of "however long
    /// construction took" (BUG-153: measuring from `Instant::now()` at
    /// construction made the first tween tick a nondeterministic, non-zero
    /// amount depending on setup time).
    motion_last_tick: Option<Instant>,
}

impl InspectorCompositePanel {
    // BUG-267 — the two canonical scopes `effects` is indexed by. `Layer`,
    // `Group`, and `Clip` all canonicalize to `SCOPE_LAYER` via `scope_idx`.
    const SCOPE_MASTER: usize = 0;
    const SCOPE_LAYER: usize = 1;

    /// Canonicalize a tab to its `effects` storage index — the single place
    /// that decides "Master effects" vs "Layer/Group/Clip effects".
    fn scope_idx(tab: InspectorTab) -> usize {
        match tab {
            InspectorTab::Master => Self::SCOPE_MASTER,
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => Self::SCOPE_LAYER,
        }
    }

    pub fn new() -> Self {
        Self {
            macros_panel: MacrosPanel::new(),
            audio_trigger_section: AudioTriggerSection::new(),
            master_chrome: MasterChromePanel::new(),
            layer_chrome: LayerChromePanel::new(),
            clip_chrome: ClipChromePanel::new(),
            effects: [Vec::new(), Vec::new()],
            gen_params: None,
            master_dying: Vec::new(),
            layer_dying: Vec::new(),
            layer_scope_id: None,
            card_context: CardContext::Perform,
            active_tab: InspectorTab::Master,
            available_tabs: vec![InspectorTab::Master],
            tab_node_ids: Vec::new(),
            collapse_all_btn_id: None,
            compact_toggle_btn_id: None,
            mods_compact: false,
            add_master_effect_btn: None,
            add_layer_effect_btn: None,
            master_scroll: ScrollContainer::new(),
            layer_scroll: ScrollContainer::new(),
            viewport_rect: Rect::ZERO,
            column_split_x: 0.0,
            columns_y: 0.0,
            columns_height: 0.0,
            dragging_scrollbar: false,
            dragging_scrollbar_master: false,
            scrolled_in_place: false,
            pressed_target: None,
            last_effect_tab: InspectorTab::Layer,
            bg_panel_id: None,
            selected_master_ids: HashSet::new(),
            selected_layer_ids: HashSet::new(),
            last_clicked_master: None,
            last_clicked_layer: None,
            card_drag_active: false,
            card_drag_tab: InspectorTab::Master,
            card_drag_source_index: 0,
            card_drag_effect_index: 0,
            card_drag_target_index: 0,
            card_drag_ghost_id: None,
            card_drag_indicator_id: None,
            card_drag_label: String::new(),
            card_drag_region_root: None,
            cache_first_node: usize::MAX,
            cache_node_count: 0,
            drawer_anim_active: false,
            drawer_anim_prev: false,
            motion_last_tick: None,
        }
    }

    /// True while any inspector card's drawer open/close tween needs another
    /// rebuild to advance (P1 motion). The app polls this after `ui_root.update()`
    /// and forces `needs_rebuild` while true — mirrors the `is_dragging()` rebuild
    /// poll. Reduced motion settles tweens instantly, so this returns false at
    /// once (no per-frame rebuild churn).
    pub fn drawer_anim_active(&self) -> bool {
        self.drawer_anim_active
    }

    /// Force every card's tweens (drawer height, tab-ink, collapse, spawn
    /// pop, delete fade, value flash, value snap-back) to their settled end
    /// state — BUG-073 fix shape (b): a headless `--script` driver has no
    /// per-frame timer, so a tween a step arms mid-script would otherwise
    /// never advance unless that step happens to insert a `Step` afterward.
    /// Returns whether anything was actually mid-flight — the caller only
    /// needs to force a rebuild (drawer heights only take effect at the next
    /// `build()`) when this is `true`.
    pub fn skip_to_settled(&mut self, tree: &mut UITree) -> bool {
        let mut any = false;
        for card in self.effects.iter_mut().flatten() {
            any |= card.skip_to_settled(tree);
        }
        if let Some(gp) = self.gen_params.as_mut() {
            any |= gp.skip_to_settled(tree);
        }
        if any {
            self.drawer_anim_active = false;
        }
        any
    }

    // ── Configuration ─────────────────────────────────────────────

    /// The scope currently shown in the inspector.
    pub fn active_tab(&self) -> InspectorTab {
        self.active_tab
    }

    // ── Section visibility — derived from the single `active_tab` ──────
    // (Master / Layer+Group / Clip partition the tab set.)
    fn master_visible(&self) -> bool {
        self.active_tab == InspectorTab::Master
    }
    fn layer_visible(&self) -> bool {
        self.active_tab.is_layer_scope()
    }
    fn clip_visible(&self) -> bool {
        self.active_tab == InspectorTab::Clip
    }


    /// Point the inspector at a single scope. `Group` shares the layer section.
    fn set_active_tab(&mut self, tab: InspectorTab) {
        // Single source of truth — visibility is derived on read, not cached.
        self.active_tab = tab;
    }

    /// Display label for a tab rung.
    fn tab_label(tab: InspectorTab) -> &'static str {
        match tab {
            InspectorTab::Clip => "Clip",
            InspectorTab::Layer => "Layer",
            InspectorTab::Group => "Group",
            InspectorTab::Master => "Master",
        }
    }



    /// §6b — push the global compact flag onto every card (master + layer effect
    /// cards and the generator-param card) so their drawers hide/show together.
    fn apply_mods_compact(&mut self) {
        let c = self.mods_compact;
        for card in self.effects.iter_mut().flatten() {
            card.set_compact(c);
        }
        if let Some(gp) = self.gen_params.as_mut() {
            gp.set_compact(c);
        }
    }

    /// Number of effect cards in the active column (master or layer/clip).
    fn active_column_card_count(&self) -> usize {
        let mut n = 0;
        if self.master_visible() {
            n += self.effects[Self::SCOPE_MASTER].len();
        }
        if self.layer_visible() || self.clip_visible() {
            n += self.effects[Self::SCOPE_LAYER].len();
        }
        n
    }

    /// True if any effect card in the active column is currently expanded — the
    /// collapse-all control collapses when this holds, expands otherwise.
    fn any_active_card_expanded(&self) -> bool {
        if self.master_visible()
            && self.effects[Self::SCOPE_MASTER].iter().any(|c| !c.is_collapsed())
        {
            return true;
        }
        if (self.layer_visible() || self.clip_visible())
            && self.effects[Self::SCOPE_LAYER].iter().any(|c| !c.is_collapsed())
        {
            return true;
        }
        false
    }






    /// Screen-space rect of the mapping-drawer chevron for `param_id`,
    /// searched across every card this panel owns (master/layer effects,
    /// generator). `None` when no card currently exposes that param as a
    /// mappable row (wrong context, not built yet, or param unknown).
    pub fn mapping_chevron_rect(&self, tree: &UITree, param_id: &str) -> Option<Rect> {
        self.effects
            .iter()
            .flatten()
            .chain(self.gen_params.iter())
            .find_map(|card| card.mapping_chevron_rect(tree, param_id))
    }


    // ── Accessors ─────────────────────────────────────────────────

    pub fn master_chrome(&self) -> &MasterChromePanel {
        &self.master_chrome
    }
    pub fn master_chrome_mut(&mut self) -> &mut MasterChromePanel {
        &mut self.master_chrome
    }
    pub fn layer_chrome(&self) -> &LayerChromePanel {
        &self.layer_chrome
    }
    pub fn layer_chrome_mut(&mut self) -> &mut LayerChromePanel {
        &mut self.layer_chrome
    }
    pub fn clip_chrome(&self) -> &ClipChromePanel {
        &self.clip_chrome
    }
    pub fn clip_chrome_mut(&mut self) -> &mut ClipChromePanel {
        &mut self.clip_chrome
    }
    pub fn gen_params(&self) -> Option<&ParamCardPanel> {
        self.gen_params.as_ref()
    }
    pub fn gen_params_mut(&mut self) -> Option<&mut ParamCardPanel> {
        self.gen_params.as_mut()
    }

    /// D9 widget catalog for the inspector's manifest-backed cards — every LIVE
    /// [`ParamCardPanel`] this panel owns (both effect scopes' cards + the
    /// generator card) as a [`CatalogSurface`], in render order. Only cards
    /// built this frame contribute affordances; the inactive scope's cards
    /// (cleared node range) and any card that minted no addressable row
    /// affordance are dropped, so the catalog lists exactly the sanctioned row
    /// surface a flow harness can reach right now. The `--catalog` dump mode
    /// serializes this. Pure enumeration over the existing per-node durable
    /// ids + names — no new protocol (each card's `catalog` does the walk).
    pub fn catalog(&self, tree: &UITree) -> Vec<crate::param_surface::CatalogSurface> {
        let mut out = Vec::new();
        for scope in &self.effects {
            for card in scope {
                let surface = card.catalog(tree);
                if !surface.affordances.is_empty() {
                    out.push(surface);
                }
            }
        }
        if let Some(gen_card) = &self.gen_params {
            let surface = gen_card.catalog(tree);
            if !surface.affordances.is_empty() {
                out.push(surface);
            }
        }
        out
    }

    /// Returns true if the effect param at `(fx_idx, param_id)` has an
    /// Ableton mapping. Keyed by stable id (Phase 2): `fx_idx` is
    /// structural (chain position), `param_id` is the unified id
    /// namespace shared across static + user-exposed bindings.
    pub fn is_effect_ableton_mapped(
        &self,
        tab: InspectorTab,
        fx_idx: usize,
        param_id: &str,
    ) -> bool {
        self.cards_for_tab(tab)
            .get(fx_idx)
            .is_some_and(|card| card.param_has_ableton_mapping(param_id))
    }

    /// Returns true if the gen param identified by `param_id` has an
    /// Ableton mapping.
    pub fn is_gen_ableton_mapped(&self, param_id: &str) -> bool {
        self.gen_params
            .as_ref()
            .is_some_and(|gp| gp.param_has_ableton_mapping(param_id))
    }

    /// Whether the effect at `(tab, fx_idx)` has diverged from its library
    /// entry (PRESET_LIBRARY_DESIGN D3/P4) — same tab-resolution as
    /// [`Self::is_effect_ableton_mapped`], read by the card context menu to
    /// gate Revert/Push to Library.
    pub fn effect_has_graph_mod(&self, tab: InspectorTab, fx_idx: usize) -> bool {
        self.cards_for_tab(tab)
            .get(fx_idx)
            .is_some_and(|card| card.has_graph_mod())
    }

    /// Whether the layer's generator has diverged from its library entry
    /// (twin of [`Self::effect_has_graph_mod`] for the single generator
    /// card).
    pub fn gen_has_graph_mod(&self) -> bool {
        self.gen_params.as_ref().is_some_and(|gp| gp.has_graph_mod())
    }

    pub fn master_effect_mut(&mut self, idx: usize) -> Option<&mut ParamCardPanel> {
        self.effects[Self::SCOPE_MASTER].get_mut(idx)
    }
    pub fn layer_effect_mut(&mut self, idx: usize) -> Option<&mut ParamCardPanel> {
        self.effects[Self::SCOPE_LAYER].get_mut(idx)
    }
    /// `master_effect_mut`/`layer_effect_mut`, picked by `tab` — mirrors
    /// `is_effect_ableton_mapped`'s Master vs Layer|Group|Clip split. The one
    /// accessor a `GraphParamTarget::Effect(idx)` dispatch arm needs when it
    /// wants to reach into the specific card's own UI-only state (e.g. P2
    /// `begin_value_snapback`) rather than just mutate the model.
    pub fn effect_card_mut(&mut self, tab: InspectorTab, idx: usize) -> Option<&mut ParamCardPanel> {
        self.cards_for_tab_mut(tab).get_mut(idx)
    }
    pub fn viewport_rect(&self) -> Rect {
        self.viewport_rect
    }
    pub fn scroll_offset(&self) -> f32 {
        self.layer_scroll.scroll_offset()
    }
    /// Which inspector tab the last effect interaction targeted.
    /// Dispatch uses this to route EffectParamChanged etc. to the
    /// correct data location (master / layer / clip effects).
    pub fn last_effect_tab(&self) -> InspectorTab {
        self.last_effect_tab
    }

    pub fn macros_panel(&self) -> &MacrosPanel {
        &self.macros_panel
    }
    pub fn macros_panel_mut(&mut self) -> &mut MacrosPanel {
        &mut self.macros_panel
    }
    pub fn macro_label_rect(&self, tree: &UITree, index: usize) -> Option<Rect> {
        self.macros_panel.label_rect(tree, index)
    }

    pub fn audio_trigger_section(&self) -> &AudioTriggerSection {
        &self.audio_trigger_section
    }
    pub fn audio_trigger_section_mut(&mut self) -> &mut AudioTriggerSection {
        &mut self.audio_trigger_section
    }

    /// D6 fire meter (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
    /// P3c, BUG-082's fix): push this tick's live shaped-signal levels onto
    /// every open fire-mode drawer's Amount meter across the inspector — in
    /// place, no rebuild (never gated behind `needs_structural_sync`; the
    /// caller runs this every UI tick regardless, mirroring
    /// `AudioSetupPanel::update_meters`/the deleted `update_trigger_levels`).
    /// Walks every card that can host a fire-mode drawer: master effects,
    /// active-layer effects, the active layer's generator, and the layer's
    /// own clip triggers. `dt` (BUG-109 P5) is the UI frame delta seconds for
    /// each meter's peak-hold timing.
    pub fn update_fire_meters(
        &self,
        tree: &mut UITree,
        fire_level: &dyn Fn(u64) -> Option<f32>,
        dt: f32,
    ) {
        for card in self.effects.iter().flatten() {
            card.update_fire_meters(tree, fire_level, dt);
        }
        if let Some(gp) = &self.gen_params {
            gp.update_fire_meters(tree, fire_level, dt);
        }
        self.audio_trigger_section.update_fire_meters(tree, fire_level, dt);
    }

    /// P7 (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 5):
    /// the send whichever fire-mode drawer is currently open across the whole
    /// inspector is reading, if any — master effects, active-layer effects,
    /// the active layer's generator, and the layer's own clip triggers, same
    /// walk order as [`Self::update_fire_meters`]. First match wins (the app
    /// doesn't let a performer open two fire-mode drawers at once today).
    pub fn open_fire_mode_drawer_send(&self) -> Option<manifold_foundation::AudioSendId> {
        for card in self.effects.iter().flatten() {
            if let Some(id) = card.open_fire_mode_drawer_send() {
                return Some(id);
            }
        }
        if let Some(gp) = &self.gen_params
            && let Some(id) = gp.open_fire_mode_drawer_send()
        {
            return Some(id);
        }
        self.audio_trigger_section.open_fire_mode_drawer_send()
    }

    /// The band whichever fire-mode drawer is currently open across the whole
    /// inspector is reading, if any — same walk order and pairing as
    /// [`Self::open_fire_mode_drawer_send`] (both read off the same open row).
    pub fn open_fire_mode_drawer_band(&self) -> Option<crate::types::AudioBand> {
        for card in self.effects.iter().flatten() {
            if let Some(b) = card.open_fire_mode_drawer_band() {
                return Some(b);
            }
        }
        if let Some(gp) = &self.gen_params
            && let Some(b) = gp.open_fire_mode_drawer_band()
        {
            return Some(b);
        }
        self.audio_trigger_section.open_fire_mode_drawer_band()
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging_scrollbar
            || self.card_drag_active
            || self.macros_panel.is_dragging()
            || self.audio_trigger_section.is_dragging()
            || self.master_chrome.is_dragging()
            || self.layer_chrome.is_dragging()
            || self.clip_chrome.is_dragging()
            || self.effects.iter().flatten().any(|e| e.is_dragging())
            || self.gen_params.as_ref().is_some_and(|p| p.is_dragging())
    }

    // ── Scrolling ─────────────────────────────────────────────────




    /// Drain the "scrolled in place this frame" signal. The app calls this once
    /// per frame and invalidates the inspector's cache slot when it returns true.
    pub fn take_scrolled_in_place(&mut self) -> bool {
        std::mem::take(&mut self.scrolled_in_place)
    }



    /// Height of the Clip section card (its own card below the layer section),
    /// or 0 when no clip is selected. BPM / warp / loop chrome lives here.
    fn clip_section_height(&self) -> f32 {
        if self.clip_visible() && self.clip_chrome.has_clip() {
            SECTION_CARD_PAD + self.clip_chrome.compute_height() + SECTION_CARD_PAD + SECTION_GAP
        } else {
            0.0
        }
    }


    fn update_scroll_bounds(&mut self) {
        self.master_scroll
            .set_content_height(self.master_column_height());
        self.layer_scroll
            .set_content_height(self.right_column_height());
    }

    fn update_scrollbar(&self, tree: &mut UITree) {
        self.master_scroll.update_scrollbar(tree);
        self.layer_scroll.update_scrollbar(tree);
    }

    // ── Tab tracking for dispatch routing ────────────────────────

    fn update_last_effect_tab(&mut self, target: &PressedTarget) {
        match target {
            PressedTarget::MasterChrome | PressedTarget::MasterEffect(_) => {
                self.last_effect_tab = InspectorTab::Master;
            }
            // The generator card is part of the layer scope (it sits in the
            // layer section, above the layer effects), so it resolves to the
            // Layer tab — not Clip.
            PressedTarget::LayerChrome
            | PressedTarget::LayerEffect(_)
            | PressedTarget::GenParam
            | PressedTarget::AudioTriggers => {
                self.last_effect_tab = InspectorTab::Layer;
            }
            PressedTarget::ClipChrome => {
                self.last_effect_tab = InspectorTab::Clip;
            }
            PressedTarget::Macros | PressedTarget::Scrollbar => {}
        }
    }

    // ── Effect selection (Unity EffectSelectionManager) ─────────


    fn last_clicked_for_tab(&self, tab: InspectorTab) -> Option<&EffectId> {
        match tab {
            InspectorTab::Master => self.last_clicked_master.as_ref(),
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => self.last_clicked_layer.as_ref(),
        }
    }

    fn set_last_clicked_for_tab(&mut self, tab: InspectorTab, id: Option<EffectId>) {
        match tab {
            InspectorTab::Master => self.last_clicked_master = id,
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => self.last_clicked_layer = id,
        }
    }





    /// Unity EffectSelectionManager.RangeSelectCards (lines 121-139)
    /// Shift+Click: range select from last clicked anchor to this index.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn range_select_effects(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() {
            return;
        }

        // Find anchor card index from the last-clicked EffectId
        let anchor = self
            .last_clicked_for_tab(tab)
            .and_then(|id| cards.iter().position(|c| c.effect_id() == id))
            .unwrap_or(0);

        let lo = anchor.min(card_index);
        let hi = anchor.max(card_index);

        // Collect IDs before mutably borrowing self
        let ids: Vec<EffectId> = (lo..=hi)
            .filter_map(|i| cards.get(i).map(|c| c.effect_id().clone()))
            .collect();

        let set = self.selection_set_mut(tab);
        set.clear();
        for id in ids {
            set.insert(id);
        }
        // Keep lastClickedIndex as anchor — do not update (Unity line 138)
    }

    /// Clear all effect selection across all tabs and repaint card borders.
    ///
    /// Takes `&mut UITree` so the card's `is_selected` flag and the tree's
    /// border style stay in lockstep. Decoupling them (set the flag now,
    /// hope a rebuild lands later) silently breaks any caller that doesn't
    /// trigger `needs_rebuild` — the cards keep their highlighted borders
    /// even though the selection set is empty, and the early-return in
    /// `update_selection_visual` then prevents future repaints from
    /// noticing the mismatch.
    pub fn clear_effect_selection(&mut self, tree: &mut UITree) {
        for tab in [
            InspectorTab::Master,
            InspectorTab::Layer,
            InspectorTab::Clip,
        ] {
            self.selection_set_mut(tab).clear();
            self.set_last_clicked_for_tab(tab, None);
        }
        for card in self.effects.iter_mut().flatten() {
            card.update_selection_visual(tree, false);
        }
    }

    /// Apply selection visuals to the tree (call after handle_event when
    /// EffectCardClicked is returned). Updates border colors without rebuild.
    /// This is the SINGLE place that syncs is_selected + tree style together.
    pub fn apply_selection_visuals(&mut self, tree: &mut UITree) {
        for tab in [
            InspectorTab::Master,
            InspectorTab::Layer,
            InspectorTab::Clip,
        ] {
            let (set, _) = self.selection_for_tab(tab);
            let set_clone: HashSet<EffectId> = set.clone();
            let cards = self.cards_for_tab_mut(tab);
            for card in cards.iter_mut() {
                let selected = set_clone.contains(card.effect_id());
                card.update_selection_visual(tree, selected);
            }
        }
    }


    /// Whether any effects are selected (for keyboard shortcut routing).
    pub fn has_effect_selection(&self) -> bool {
        !self.selected_master_ids.is_empty() || !self.selected_layer_ids.is_empty()
    }

    /// Get all selected effect indices for the active tab.
    /// Converts selected EffectIds back to card indices (sorted ascending).
    /// Commands (delete, reorder) still operate on indices.
    pub fn get_selected_effect_indices(&self) -> Vec<usize> {
        let (set, cards) = self.selection_for_tab(self.last_effect_tab);
        let mut indices: Vec<usize> = cards
            .iter()
            .enumerate()
            .filter(|(_, card)| set.contains(card.effect_id()))
            .map(|(i, _)| i)
            .collect();
        indices.sort_unstable();
        indices
    }


    // ── Node range ownership ─────────────────────────────────────




    // ── Drag event routing (needs &mut UITree) ───────────────────

    /// Whether a sub-panel is currently pressed (active drag target).
    pub fn has_pressed_target(&self) -> bool {
        self.pressed_target.is_some() || self.dragging_scrollbar || self.card_drag_active
    }





    // ── Effect card drag-reorder (Unity EffectsListBitmapPanel) ──







    // ── Internal event routing ───────────────────────────────────

    /// Check if an effect target is already part of the current selection.
    fn is_effect_target_selected(&self, target: &PressedTarget) -> bool {
        match *target {
            PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                .get(i)
                .is_some_and(|c| self.selected_master_ids.contains(c.effect_id())),
            PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                .get(i)
                .is_some_and(|c| self.selected_layer_ids.contains(c.effect_id())),
            _ => false,
        }
    }

    /// Auto-select an effect card on any interaction (click, pointer down).
    /// Unity: any card interaction implicitly selects it (single-select, no modifiers).
    fn auto_select_effect(&mut self, target: &PressedTarget) {
        match *target {
            PressedTarget::MasterEffect(i) => self.select_effect(InspectorTab::Master, i),
            PressedTarget::LayerEffect(i) => self.select_effect(InspectorTab::Layer, i),
            _ => {}
        }
    }



}

impl Panel for InspectorCompositePanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.build_in_rect(tree, layout.inspector());
    }

    fn update(&mut self, tree: &mut UITree) {
        // State sync is done via direct accessors on sub-panels.
        // The app layer calls sync methods like:
        //   inspector.master_chrome_mut().sync_opacity(&mut tree, 0.5);

        // P1 drawer motion: advance every card's drawer-height tween. This only
        // moves the tween *values*; the reflow they drive happens on the next
        // build() (which the app's drawer_anim_active poll forces while a tween is
        // live). No tree mutation here — build reads the advanced values.
        let dt_ms = self
            .motion_last_tick
            .map(|t| (t.elapsed().as_secs_f32() * 1000.0).min(100.0))
            .unwrap_or(0.0);
        self.motion_last_tick = Some(Instant::now());
        let mut any = false;
        for card in self.effects.iter_mut().flatten() {
            any |= card.tick_drawers(dt_ms);
            // P2 value-change flash + D1 tab-ink slide's ink tween both live in
            // this same per-param vocabulary; the flash needs `tree` (a style
            // write, not a layout change), so unlike drawer/ink it never sets
            // `any` — it doesn't need the forced-rebuild poll, only to run
            // every frame, which this loop already does.
            card.tick_value_flash(tree, dt_ms);
        }
        if let Some(gp) = self.gen_params.as_mut() {
            any |= gp.tick_drawers(dt_ms);
            gp.tick_value_flash(tree, dt_ms);
        }
        // D17 "delete collapse" (exit-state pattern) — dying cards keep
        // ticking (and forcing the rebuild that reflows what follows them)
        // until their collapse+fade finishes, then get dropped for good. The
        // data model already forgot these; this only controls how long the
        // UI keeps painting a card it no longer has.
        for card in self.master_dying.iter_mut().chain(self.layer_dying.iter_mut()) {
            any |= card.tick_drawers(dt_ms);
        }
        self.master_dying.retain(|c| !c.is_delete_finished());
        self.layer_dying.retain(|c| !c.is_delete_finished());
        // Stay "active" one extra frame after the last tween settles so its final
        // (target) value gets a build to render — the settling tick returns false
        // but its new value hasn't reached the screen yet.
        self.drawer_anim_active = any || self.drawer_anim_prev;
        self.drawer_anim_prev = any;
    }

    fn handle_event(&mut self, event: &UIEvent, tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click {
                node_id,
                pos,
                modifiers,
            } => {
                if !self.viewport_rect.contains(*pos) {
                    return Vec::new();
                }
                // The driver Free-period field opens a type-in (needs `tree` for
                // its anchor), so intercept it before the command-routing click.
                let typein = self.route_driver_period_typein(*node_id, tree);
                if !typein.is_empty() {
                    return typein;
                }
                self.route_click(*node_id, *modifiers, tree)
            }
            UIEvent::PointerDown {
                node_id,
                pos,
                modifiers,
            } => {
                if !self.viewport_rect.contains(*pos) {
                    return Vec::new();
                }
                self.route_pointer_down(*node_id, *pos, *modifiers, tree)
            }
            UIEvent::DoubleClick { node_id, pos, .. } => {
                if !self.viewport_rect.contains(*pos) {
                    return Vec::new();
                }
                self.route_value_typein(*node_id, tree)
            }
            UIEvent::DragBegin { node_id, pos, .. } => {
                if !self.viewport_rect.contains(*pos) {
                    return Vec::new();
                }
                // DragBegin only ensures pressed_target is set for drag routing.
                // Do NOT re-call route_pointer_down — that re-fires
                // handle_pointer_down on the sub-panel, overwriting the undo
                // snapshot captured on PointerDown. Unity's DragBegin just starts
                // routing Drag events; it doesn't re-apply the slider value.
                // `node_id` is `Option` (D9): `None` means the pressed node died
                // before the drag threshold crossed. `pressed_target` is normally
                // already set from PointerDown (unaffected by D9, still gated on
                // a live node there) — a `None` here just means no NEW target can
                // be resolved, same as a `Some` id resolving to no target.
                if self.pressed_target.is_none()
                    && let Some(node_id) = *node_id
                {
                    let target = self.find_target_for_node(node_id);
                    self.pressed_target = target;
                    if let Some(ref t) = target {
                        self.update_last_effect_tab(t);
                    }
                }
                Vec::new()
            }
            // Right-click is handled entirely by node-intent dispatch (see
            // `register_intents` below); the inspector no longer routes it.
            _ => Vec::new(),
        }
    }

    /// Forward node-intent registration to the param cards and chrome
    /// sub-panels. Right-clicks now resolve through intent dispatch (with
    /// fold-up over dead zones) instead of `route_right_click`'s exact-id
    /// matching. (clip_chrome has no right-click affordance, so nothing to
    /// register.) See `docs/NODE_INTENT_DISPATCH.md`.
    fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        // Only the active scope built nodes this frame; an inactive scope's cards
        // hold stale ids that now belong to the active content, so registering
        // their intents would bind right-clicks on live nodes to phantom targets.
        // Liveness is the node range: param cards self-guard (a non-live card's
        // `register_intents` no-ops), and the chrome sections are gated on
        // `node_count() > 0` here — one signal, the same the rest of the panel uses.
        self.macros_panel.register_intents(intents);
        self.audio_trigger_section.register_intents(intents);
        if self.master_chrome.node_count() > 0 {
            self.master_chrome.register_intents(intents);
        }
        for card in &self.effects[Self::SCOPE_MASTER] {
            card.register_intents(intents);
        }
        if self.layer_chrome.node_count() > 0 {
            self.layer_chrome.register_intents(intents);
        }
        if let Some(gp) = self.gen_params.as_ref() {
            gp.register_intents(intents);
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            card.register_intents(intents);
        }
    }

    fn first_node(&self) -> usize {
        self.cache_first_node
    }
    fn node_count(&self) -> usize {
        self.cache_node_count
    }
}

impl Default for InspectorCompositePanel {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn in_range(idx: usize, first: usize, count: usize) -> bool {
    count > 0 && idx >= first && idx < first + count
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::param_card::{RelightCardConfig, RowMod};
    use crate::tree::UITree;

    fn inspector_layout() -> ScreenLayout {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 280.0;
        layout
    }

    fn close(a: Rect, b: Rect) -> bool {
        (a.x - b.x).abs() < 0.01
            && (a.y - b.y).abs() < 0.01
            && (a.width - b.width).abs() < 0.01
            && (a.height - b.height).abs() < 0.01
    }

    #[test]
    fn section_card_view_matches_old_pixel_math() {
        // The typed section-card view must materialise to the exact two panels
        // the old hand-rolled border+bg add_panel pair produced.
        let mut tree = UITree::new();
        let before = tree.count() as u32;
        let rect = Rect::new(10.0, 20.0, 200.0, 100.0);
        chrome::materialize(&mut tree, &section_card_view(), rect);

        assert_eq!(tree.count() as u32, before + 2, "border + inner-bg panels");
        let border = tree.get_node(tree.id_at(before as usize)).unwrap();
        assert!(close(border.bounds, rect), "border at the card rect");
        assert_eq!(border.style.bg_color, SECTION_CARD_BORDER);
        assert_eq!(border.style.corner_radius, SECTION_CARD_RADIUS);
        let bg = tree.get_node(tree.id_at(before as usize + 1)).unwrap();
        assert!(
            close(bg.bounds, Rect::new(11.0, 21.0, 198.0, 98.0)),
            "inner bg inset 1px: {:?}",
            bg.bounds
        );
        assert_eq!(bg.style.bg_color, SECTION_CARD_BG);
        assert_eq!(bg.style.corner_radius, SECTION_CARD_RADIUS - 1.0);
    }

    #[test]
    fn add_effect_button_view_matches_old_pixel_math() {
        let mut tree = UITree::new();
        let rect = Rect::new(5.0, 5.0, 150.0, ADD_EFFECT_BTN_H);
        let ids = chrome::materialize(&mut tree, &add_effect_button_view(), rect);

        let btn_id = ids
            .iter()
            .find(|(k, _)| *k == KEY_ADD_EFFECT_BTN)
            .map(|(_, id)| *id)
            .expect("button id recovered by key");
        let btn = tree.get_node(btn_id).unwrap();
        assert!(close(btn.bounds, rect), "button at the given rect");
        assert_eq!(btn.text.as_deref(), Some("+ Add Effect"));
        assert_eq!(btn.node_type, UINodeType::Button);
        assert!(btn.flags.contains(UIFlags::INTERACTIVE));
        assert_eq!(btn.style.bg_color, chrome::components::button_secondary_style().bg_color);
    }

    #[test]
    fn build_empty_inspector() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        assert!(panel.bg_panel_id.is_some());
        assert!(panel.layer_scroll.track_id().is_some());
        assert!(panel.layer_scroll.thumb_id().is_some());
        assert!(tree.count() > 0);
    }

    #[test]
    fn build_with_zero_inspector_width() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 0.0;
        panel.build(&mut tree, &layout);

        // Nothing built when inspector is closed
        assert_eq!(panel.bg_panel_id, None);
    }

    #[test]
    fn scroll_clamps_to_bounds() {
        let mut panel = InspectorCompositePanel::new();
        // Set up a scroll region with content taller than viewport.
        // ScrollContainer needs a viewport to compute max_scroll.
        let mut tree = UITree::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);
        // Manually set content height to create a scrollable range.
        panel
            .layer_scroll
            .set_content_height(panel.layer_scroll.viewport().height + 100.0);

        panel.handle_scroll(-100.0); // scroll way down
        assert!(panel.layer_scroll.scroll_offset() <= panel.layer_scroll.max_scroll());

        panel.handle_scroll(100.0); // scroll way up
        assert!(panel.layer_scroll.scroll_offset() >= 0.0);
    }

    #[test]
    fn active_tab_drives_section_visibility() {
        let mut panel = InspectorCompositePanel::new();

        // Master active → only the master section renders.
        panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        assert!(panel.master_visible());
        assert!(!panel.layer_visible());
        assert!(!panel.clip_visible());

        // Layer active → only the layer section.
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        assert!(!panel.master_visible());
        assert!(panel.layer_visible());
        assert!(!panel.clip_visible());

        // Group shares the layer section (a group is a layer).
        panel.configure_tabs(
            &[InspectorTab::Group, InspectorTab::Master],
            InspectorTab::Group,
        );
        assert!(panel.layer_visible());
        assert!(!panel.master_visible());
        assert!(!panel.clip_visible());

        // Clip active → only the clip section.
        panel.configure_tabs(
            &[InspectorTab::Clip, InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Clip,
        );
        assert!(panel.clip_visible());
        assert!(!panel.master_visible());
        assert!(!panel.layer_visible());
        assert_eq!(panel.active_tab(), InspectorTab::Clip);
    }

    fn mk_param(id: &'static str, name: &str) -> crate::param_surface::ParamRow {
        use crate::param_surface::{ParamRow, RowMapping, RowSpec, RowValue};
        ParamRow {
            id: std::borrow::Cow::Borrowed(id),
            spec: RowSpec {
                name: name.into(),
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
        }
    }

    fn mk_config(kind: super::super::param_card::ParamCardKind, name: &str, n: usize) -> ParamSurface {
        // Unique id per row (D4, `docs/WIDGET_TREE_DESIGN.md`): a real
        // manifest never repeats a `ParamId` within one card, and P2 keys
        // every row widget off it — the old `["a","b","c","d"][i % 4]` cycle
        // synthesized a duplicate-id collision no real card can produce, once
        // `n > 4` (caught by `drag_hit_test_target_index_with_mixed_height_cards`).
        let rows: Vec<_> = (0..n)
            .map(|i| {
                let mut row = mk_param(["a", "b", "c", "d"][i % 4], &format!("P{i}"));
                row.id = std::borrow::Cow::Owned(format!("row{i}"));
                row
            })
            .collect();
        ParamSurface {
            kind,
            title: name.into(),
            rows,
            string_params: vec![],
            collapsed: false,
            effect_index: 0,
            // Real id for both kinds (fixed 2026-07-11): a populated
            // generator card carries `inst.id` in production now, never a
            // blanked `EffectId::new("")` — this mirrors that for both arms
            // instead of modeling the pre-fix shape.
            effect_id: EffectId::new(name),
            enabled: true,
            supports_envelopes: true,
            has_graph_mod: false,
            layer_id: None,
            audio: Default::default(),
            relight: RelightCardConfig::default(),
        }
    }

    /// Range truthfulness: switching scope must reset the inactive section's
    /// node range to empty, not leave it pointing at the frame it was last built.
    /// `build` clears every section up front, so an un-built scope reports
    /// `first_node == usize::MAX` / `node_count == 0` — and `sub_region_ranges()`
    /// (and every other range consumer) then naturally excludes it without a
    /// `*_visible()` gate. Before this, the stale master_chrome range overlapped
    /// the active scope's nodes and leaked into the cache's incremental list and
    /// the intent registry (phantom right-click targets).
    #[test]
    fn subregions_exclude_inactive_scope_after_scope_switch() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };

        // Frame 1: master active → builds the master chrome (records a range).
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "MasterFX", 2)]);
        panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        tree.clear();
        panel.build(&mut tree, &layout);
        assert!(
            panel.master_chrome.node_count() > 0,
            "master chrome should have built"
        );

        // Frame 2: layer active (gen + layer effect). Master chrome is NOT built,
        // so the up-front reset must leave its range empty (not stale).
        panel.configure_layer_effects(&[mk_config(ParamCardKind::Effect, "LayerFX", 2)], None);
        let mut text_gen = mk_config(ParamCardKind::Generator, "Text", 3);
        text_gen.string_params = vec![super::super::param_card::ParamCardStringInfo {
            name: "Text".into(),
            key: "text".into(),
            value: "HELLO".into(),
            use_dropdown: false,
        }];
        panel.configure_gen_params(Some(&text_gen), None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        tree.clear();
        panel.build(&mut tree, &layout);

        // The inactive master chrome reports a reset (empty) range — the stale
        // range can no longer exist to be confused for live layer nodes.
        assert_eq!(
            panel.master_chrome.first_node(),
            usize::MAX,
            "inactive scope must reset first_node to the not-built sentinel"
        );
        assert_eq!(
            panel.master_chrome.node_count(),
            0,
            "inactive scope must report zero nodes"
        );

        // The gen card IS covered (active scope), and no sub-region overlaps the
        // (now empty) master section — nothing points the cache at live layer nodes.
        let genp = panel.gen_params.as_ref().unwrap();
        let gen_range = (genp.first_node(), genp.first_node() + genp.node_count());
        let subs = panel.sub_region_ranges();
        assert!(
            subs.iter().any(|&(s, e)| s <= gen_range.0 && gen_range.1 <= e),
            "gen card must be covered: gen={gen_range:?} subs={subs:?}"
        );
        // Every reported range is a real, live (non-empty) range.
        assert!(
            subs.iter().all(|&(s, e)| s != usize::MAX && e > s),
            "no empty/sentinel ranges may be reported: {subs:?}"
        );
    }

    /// Regression: the generator card is built, sized, and range-registered
    /// under the LAYER section (gated on `layer_visible`), but `find_target_for_node`
    /// used to hit-test it under the CLIP section (gated on `clip_visible`). Because
    /// the inspector tabs are mutually exclusive, the Layer tab leaves
    /// `clip_visible == false`, so every click / pointer-down / drag on the gen card
    /// resolved to `None` and was dropped — the card rendered but was completely
    /// dead. Every node in the gen card's range must resolve to `GenParam`.
    #[test]
    fn gen_card_is_hit_testable_on_layer_tab() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };

        let mut text_gen = mk_config(ParamCardKind::Generator, "Text", 3);
        text_gen.string_params = vec![super::super::param_card::ParamCardStringInfo {
            name: "Text".into(),
            key: "text".into(),
            value: "HELLO".into(),
            use_dropdown: false,
        }];
        panel.configure_gen_params(Some(&text_gen), None);
        // Layer tab active → layer_visible, clip_visible == false (the regression
        // case: no clip selected, so the clip section is gone entirely).
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        tree.clear();
        panel.build(&mut tree, &layout);

        let gp = panel.gen_params.as_ref().expect("gen card configured");
        let (first, count) = (gp.first_node(), gp.node_count());
        assert!(count > 0, "gen card should have built nodes");

        for i in first..first + count {
            let target = panel.find_target_for_node(tree.id_at(i));
            assert!(
                matches!(target, Some(PressedTarget::GenParam)),
                "gen-card node {i} must route to GenParam on the Layer tab, got {target:?}",
            );
        }
    }

    /// BUG-121 host-level regression: a graph-editor-window inspector
    /// (`set_card_context(Author)`, mirroring `Workspace::new`'s wiring)
    /// must draw a resolvable mapping-drawer chevron for a mappable param —
    /// and a main-window inspector (default `Perform`) must not. Guards
    /// against the exact live gap this bug shipped with: `set_context`
    /// itself worked (covered by `param_card.rs`'s own unit tests), but no
    /// production host ever called it, so the drawer was unreachable
    /// app-wide despite the widget code being correct in isolation.
    #[test]
    fn author_context_host_draws_resolvable_mapping_chevron() {
        use super::super::param_card::ParamCardKind;
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };

        let mut config = mk_config(ParamCardKind::Effect, "Mirror", 2);
        config.rows[1].mapping.mappable = true;
        let mappable_id = config.rows[1].id.to_string();

        // Author-context host (the graph-editor window's inspector).
        let mut author_tree = UITree::new();
        let mut author_panel = InspectorCompositePanel::new();
        author_panel.set_card_context(CardContext::Author);
        author_panel.configure_master_effects(&[config.clone()]);
        author_panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        author_panel.build(&mut author_tree, &layout);
        assert!(
            author_panel
                .mapping_chevron_rect(&author_tree, &mappable_id)
                .is_some(),
            "Author-context host must draw a resolvable mapping chevron"
        );

        // Perform-context host (the main window's inspector, the default).
        let mut perform_tree = UITree::new();
        let mut perform_panel = InspectorCompositePanel::new();
        perform_panel.configure_master_effects(&[config]);
        perform_panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        perform_panel.build(&mut perform_tree, &layout);
        assert!(
            perform_panel
                .mapping_chevron_rect(&perform_tree, &mappable_id)
                .is_none(),
            "Perform-context host must never draw the mapping chevron"
        );
    }

    /// `GRAPH_EDITOR_INSPECTOR_UNIFICATION.md` Change 4, D5 (superseded by the
    /// BUG-160 follow-up): the same fixture built into a Perform-context and
    /// an Author-context panel at the SAME rect must place its param LABEL
    /// identically — only the chevron lane, and therefore the slider track
    /// width, differs (Author reserves it, Perform doesn't, since Perform
    /// never draws the glyph). The label rect is upstream of that lane, so it
    /// can't drift regardless of which context reserves the chevron. Narrower
    /// in scope than a full tree-equivalence walk (that's the
    /// `inspector_rows_fit_card_bounds_across_widths` width-sweep test still
    /// owed, per the design doc's P1 deliverables) — this proves the shared
    /// `row_geometry` label math (D2) rather than every possible future
    /// divergence.
    #[test]
    fn perform_and_author_slider_row_labels_are_geometry_identical() {
        use super::super::param_card::ParamCardKind;
        let rect = Rect::new(0.0, 0.0, 400.0, 600.0);

        let config = mk_config(ParamCardKind::Effect, "Mirror", 2);
        let param_id = config.rows[0].id.to_string();

        let mut author_tree = UITree::new();
        let mut author_panel = InspectorCompositePanel::new();
        author_panel.set_card_context(CardContext::Author);
        author_panel.configure_master_effects(std::slice::from_ref(&config));
        author_panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        author_panel.build_in_rect(&mut author_tree, rect);
        let author_row = author_panel
            .master_effect_mut(0)
            .expect("author card configured")
            .param_row_rect(&author_tree, &param_id)
            .expect("author row built");

        let mut perform_tree = UITree::new();
        let mut perform_panel = InspectorCompositePanel::new();
        perform_panel.configure_master_effects(&[config]);
        perform_panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        perform_panel.build_in_rect(&mut perform_tree, rect);
        let perform_row = perform_panel
            .master_effect_mut(0)
            .expect("perform card configured")
            .param_row_rect(&perform_tree, &param_id)
            .expect("perform row built");

        assert_eq!(
            author_row, perform_row,
            "the same fixture at the same rect must place its param label \
             identically in both contexts — the chevron lane only affects \
             the slider track, not the label upstream of it"
        );
    }

    /// Regression: add-effect button ids are reassigned by node index every
    /// rebuild, but each is only *set* inside its own `!collapsed`/`*_visible`
    /// build branch. When a section stops being built (tab switch, collapse),
    /// the stale id persisted and — because the exact-id checks in `route_click`
    /// run before the range-based `find_target_for_node` — could shadow whatever
    /// node now occupies that index. Concretely: the generator card's Change
    /// button inheriting a stale add-effect id, opening the effect browser
    /// instead of the generator picker. The ids must clear on every build.
    #[test]
    fn stale_add_effect_button_id_cleared_when_section_hidden() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };

        // Master tab with master effects → the master add-effect button builds.
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "MasterFX", 2)]);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Master,
        );
        tree.clear();
        panel.build(&mut tree, &layout);
        assert!(
            panel.add_master_effect_btn.is_some(),
            "master add-effect button must be registered on the Master tab",
        );

        // Switch to the Layer tab: the master section is no longer built, so its
        // add-effect button is gone. The stale id must not survive the rebuild.
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        tree.clear();
        panel.build(&mut tree, &layout);
        assert!(
            panel.add_master_effect_btn.is_none(),
            "stale master add-effect id must clear when the section is hidden",
        );
    }

    /// BUG-108 class-kill (I3, docs/UI_LAYOUT_INVARIANT_LINTS_PROPOSAL.md): the
    /// "+ Add Effect" button must never overlap a sectioned layer effect card's
    /// last row. This is the exact defect Peter hit on the rig — a glTF-scene
    /// effect card's SCENE_BUILD P3 section headers (QS1694/Material.001/
    /// Camera/Sun/Environment) inflated the card's real drawn height beyond
    /// what `compute_height()` (pre-fix) reported, so the button — anchored at
    /// `layer_column_height()`, which sums each card's `compute_height()` —
    /// landed mid-card, over the Sun Y/Sun Z rows. Reads REAL painted bounds
    /// from the tree (`param_row_rect`/button bounds), not the height formula
    /// itself, so a future drift between the formula and the draw loop fails
    /// this test even if `compute_height()` alone still agrees with itself.
    #[test]
    fn add_effect_button_does_not_overlap_sectioned_card_last_row() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };

        // A layer effect card whose LAST two rows are grouped under a "Sun"
        // section header — the reported card's exact shape (a section run
        // covering the card's tail, header stacked above its own rows).
        let mut config = mk_config(ParamCardKind::Effect, "SceneFX", 4);
        config.rows[2].spec.section = Some("Sun".to_string());
        config.rows[3].spec.section = Some("Sun".to_string());
        let last_param_id = config.rows[3].id.to_string();
        panel.configure_layer_effects(&[config], None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );

        tree.clear();
        panel.build(&mut tree, &layout);

        let btn_id = panel
            .add_layer_effect_btn
            .expect("layer add-effect button must build below the sectioned card");
        let btn_bounds = tree.get_bounds(btn_id);

        let card = panel.effects[InspectorCompositePanel::SCOPE_LAYER]
            .last()
            .expect("the sectioned layer effect card must have built");
        assert!(
            !card.section_header_ids().is_empty(),
            "sanity: the Sun run must draw its section header, or this test proves nothing"
        );
        let last_row = card
            .param_row_rect(&tree, &last_param_id)
            .expect("the sectioned card's last param row must build (unfolded by default)");

        assert!(
            btn_bounds.y + 0.5 >= last_row.y + last_row.height,
            "+ Add Effect (y={}) must sit at or below the sectioned card's last \
             painted row (bottom={}), not overlap it — BUG-108",
            btn_bounds.y,
            last_row.y + last_row.height,
        );
    }

    /// The full-panel render path (`render_tree_range` → `traverse_range`) walks
    /// only roots in range and descends. After the layer column's content is
    /// reparented under the scroll clip, EVERY visible inspector node must still
    /// be reachable from an in-range root — otherwise the post-`invalidate_all`
    /// full render silently drops it and the card body renders blank.
    #[test]
    fn full_render_traversal_reaches_every_visible_node() {
        use super::super::param_card::ParamCardKind;
        use crate::tree::TraversalEvent;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };
        panel.configure_layer_effects(&[mk_config(ParamCardKind::Effect, "LayerFX", 2)], None);
        let mut text_gen = mk_config(ParamCardKind::Generator, "Text", 3);
        text_gen.string_params = vec![super::super::param_card::ParamCardStringInfo {
            name: "Text".into(),
            key: "text".into(),
            value: "HELLO".into(),
            use_dropdown: false,
        }];
        panel.configure_gen_params(Some(&text_gen), None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        // Post-D1/D4: `ui_root.rs` now wraps the inspector's build in a
        // region (`begin_region` .. `end_region`) instead of letting it root
        // itself at the tree — mirror that here so `traverse_range` (which
        // walks registered regions, not a raw root scan) actually reaches
        // this panel's content.
        let region = tree.begin_region(
            layout.inspector(),
            crate::tree::ZTier::Base,
            "inspector",
            UIFlags::empty(),
        );
        let content_start = tree.count();
        panel.build(&mut tree, &layout);
        tree.end_region(region, content_start);

        let start = region.root.index();
        let end = tree.count();

        // Collect node indices the full-render traversal actually visits.
        let mut visited = std::collections::HashSet::new();
        tree.traverse_range(start, end, |ev| {
            if let TraversalEvent::Node(n) = ev {
                visited.insert(n.id.index());
            }
        });

        // Every VISIBLE node with a non-zero area in the inspector range must be
        // visited (these are the nodes the GPU would draw).
        let mut missed = Vec::new();
        for i in start..end {
            let n = tree.get_node(tree.id_at(i)).unwrap();
            let drawable = n.flags.contains(UIFlags::VISIBLE)
                && n.bounds.width > 0.0
                && n.bounds.height > 0.0;
            if drawable && !visited.contains(&i) {
                missed.push((i, n.node_type, n.bounds));
            }
        }
        assert!(
            missed.is_empty(),
            "full-render traversal skipped {} drawable node(s): {:?}",
            missed.len(),
            missed
        );

        // Replicate the GPU cull: track the intersected clip stack exactly as
        // `UIRenderer::handle_push_clip` + `draw_node` do, and assert no drawable
        // gen-card node is culled by its EFFECTIVE clip. This catches a tight or
        // wrong CLIPS_CHILDREN ancestor (section card / card frame) that would
        // leave the header drawn but the body blank — which counting visited
        // nodes alone cannot detect.
        fn intersect(a: Rect, b: Rect) -> Rect {
            let x0 = a.x.max(b.x);
            let y0 = a.y.max(b.y);
            let x1 = (a.x + a.width).min(b.x + b.width);
            let y1 = (a.y + a.height).min(b.y + b.height);
            Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
        }
        let genp = panel.gen_params.as_ref().unwrap();
        let (gs, gc) = (genp.first_node(), genp.node_count());
        let mut clip_stack: Vec<Rect> = Vec::new();
        let mut culled: Vec<(usize, crate::node::Rect)> = Vec::new();
        tree.traverse_range(start, end, |ev| match ev {
            TraversalEvent::PushClip(r) => {
                let clipped = clip_stack.last().map(|c| intersect(*c, r)).unwrap_or(r);
                clip_stack.push(clipped);
            }
            TraversalEvent::PopClip => {
                clip_stack.pop();
            }
            TraversalEvent::Node(n) => {
                let i = n.id.index();
                if i < gs || i >= gs + gc {
                    return;
                }
                let b = n.bounds;
                if !n.flags.contains(UIFlags::VISIBLE) || b.width <= 0.0 || b.height <= 0.0 {
                    return;
                }
                if let Some(c) = clip_stack.last() {
                    let out = b.x >= c.x + c.width
                        || b.x + b.width <= c.x
                        || b.y >= c.y + c.height
                        || b.y + b.height <= c.y;
                    if out {
                        culled.push((i, b));
                    }
                }
            }
        });
        assert!(
            culled.is_empty(),
            "{} gen-card node(s) culled by their effective clip (body would render blank): {:?}",
            culled.len(),
            culled
        );
    }

    /// `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1 stopgap decision (§ "The
    /// stopgap removal"): the bespoke, root-parented `content_clip_id`
    /// (`ClipRegion` at `(rect.x, columns_y, rect.width, columns_h)`) is
    /// gone — decided empirically, not by reading. With the outer
    /// `begin_region` clip now covering the WHOLE inspector rect (D1, wired
    /// in `ui_root.rs`, not reachable from this manifold-ui-only test)
    /// guaranteeing footer containment regardless, the open question was
    /// whether `content_clip_id` was ALSO the only thing keeping scrolled
    /// column content off the pinned macros/tab strip above `columns_y` — a
    /// DIFFERENT edge the outer region can't fence, since `columns_y` sits
    /// strictly inside it, not at its boundary.
    ///
    /// It wasn't. `master_scroll`/`layer_scroll` (`ScrollContainer::begin`,
    /// just below) each mint their OWN `CLIPS_CHILDREN` clip starting at the
    /// SAME `columns_y` — `content_clip_id`'s Y-range was always a strict
    /// subset of whichever column is active, so it was dead weight even
    /// before this design existed. Proved with a controlled experiment
    /// (`content_clip_id`'s `CLIPS_CHILDREN` flag temporarily removed,
    /// nothing else changed): scroll the layer column hard past its max
    /// (the same `try_scroll_in_place` real mouse-wheel input drives), so
    /// several cards' raw bounds land above `columns_y` — squarely where
    /// the tab strip is drawn — then replicate the GPU cull (same pattern
    /// as `full_render_traversal_reaches_every_visible_node` above) and
    /// confirm zero pixels reached the tab strip regardless. This test now
    /// guards the SIMPLER code that experiment justified: no `content_clip`
    /// at all, `layer_scroll`'s own clip carrying the whole load.
    #[test]
    fn layer_scroll_clip_prevents_scrolled_columns_painting_over_the_tab_strip() {
        use super::super::param_card::ParamCardKind;
        use crate::tree::TraversalEvent;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };
        // Nine effects, several params each — enough stacked card height to
        // scroll a long way, the same shape as the BUG-060 gate scene
        // (`ui_snapshot/fixtures.rs`'s `bug060_scene`).
        let configs: Vec<_> = (0..9)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 4))
            .collect();
        panel.configure_layer_effects(&configs, None);
        panel.configure_tabs(&[InspectorTab::Layer, InspectorTab::Master], InspectorTab::Layer);
        panel.build(&mut tree, &layout);

        let columns_y = panel.columns_y;
        assert!(columns_y > 0.0, "sanity: columns_y must be a real screen position");

        // Scroll far past any real max — `apply_scroll_delta`/`clamp_scroll`
        // clamp it, so this just guarantees "fully scrolled", not overshoot.
        let cursor_x = layout.inspector().x + layout.inspector().width - 10.0; // right column
        let scrolled = panel.try_scroll_in_place(1_000_000.0, cursor_x, &mut tree);
        assert!(scrolled, "sanity: the panel must report a live scroll container");

        let start = panel.first_node();
        let end = start + panel.node_count();

        // Sanity: the scroll must have actually moved SOME node's raw bounds
        // above columns_y — otherwise this test would vacuously pass with
        // nothing to clip (not enough content to overflow).
        let mut any_above = false;
        for i in start..end {
            let n = tree.get_node(tree.id_at(i)).unwrap();
            if n.flags.contains(UIFlags::VISIBLE)
                && n.bounds.width > 0.0
                && n.bounds.height > 0.0
                && n.bounds.y < columns_y
            {
                any_above = true;
                break;
            }
        }
        assert!(
            any_above,
            "sanity: scrolling must push some node's raw bounds above columns_y \
             ({columns_y}) or this test proves nothing — increase the effect count"
        );

        // Replicate the GPU cull exactly as the sibling test above does.
        fn intersect(a: Rect, b: Rect) -> Rect {
            let x0 = a.x.max(b.x);
            let y0 = a.y.max(b.y);
            let x1 = (a.x + a.width).min(b.x + b.width);
            let y1 = (a.y + a.height).min(b.y + b.height);
            Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
        }
        let mut clip_stack: Vec<Rect> = Vec::new();
        let mut painted_over_tab_strip: Vec<(usize, Rect)> = Vec::new();
        tree.traverse_range(start, end, |ev| match ev {
            TraversalEvent::PushClip(r) => {
                let clipped = clip_stack.last().map(|c| intersect(*c, r)).unwrap_or(r);
                clip_stack.push(clipped);
            }
            TraversalEvent::PopClip => {
                clip_stack.pop();
            }
            TraversalEvent::Node(n) => {
                let b = n.bounds;
                if !n.flags.contains(UIFlags::VISIBLE) || b.width <= 0.0 || b.height <= 0.0 {
                    return;
                }
                // Only care about nodes whose RAW bounds reach above
                // columns_y — those are the ones a missing/weakened
                // content_clip would let bleed into the tab strip.
                if b.y >= columns_y {
                    return;
                }
                let Some(c) = clip_stack.last() else {
                    // No active clip ancestor at all — definitely paints
                    // wherever its raw bounds say, tab strip included.
                    painted_over_tab_strip.push((n.id.index(), b));
                    return;
                };
                // Effective clip must not extend above columns_y — if it
                // does, this node's visible (unclipped) portion still
                // reaches above the tab strip's lower edge.
                if c.y < columns_y - 0.01 {
                    painted_over_tab_strip.push((n.id.index(), b));
                }
            }
        });
        assert!(
            painted_over_tab_strip.is_empty(),
            "{} node(s) with raw bounds above columns_y ({columns_y}) are not fully \
             clipped there — scrolled content would paint over the pinned tab strip: {:?}",
            painted_over_tab_strip.len(),
            painted_over_tab_strip
        );
    }

    /// In-place inspector scroll: offsets the live content nodes by the scroll
    /// delta WITHOUT a rebuild, and lands them exactly where a fresh rebuild at
    /// the same offset would — so scrolling stays cheap and a later rebuild never
    /// jumps. Guards the fix for the scroll-blank churn.
    #[test]
    fn in_place_scroll_matches_rebuild() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 400.0;
            l
        };
        // Many effects so the layer column overflows the viewport (scrollable).
        let effects: Vec<_> = (0..12)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 3))
            .collect();
        panel.configure_layer_effects(&effects, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);
        assert!(
            panel.layer_scroll.max_scroll() > 0.0,
            "test needs scrollable content"
        );

        // A scroll node's y before scrolling.
        let probe = panel.layer_chrome.first_node();
        let y_before = tree.get_node(tree.id_at(probe)).unwrap().bounds.y;
        let off_before = panel.layer_scroll.scroll_offset();

        // Scroll down (negative wheel delta raises the offset). Cursor anywhere
        // in the inspector routes to the layer column when it's the active scope.
        let cursor_x = layout.inspector().x + 10.0;
        let handled = panel.try_scroll_in_place(-30.0, cursor_x, &mut tree);
        assert!(handled, "built inspector must scroll in place");

        let off_after = panel.layer_scroll.scroll_offset();
        assert!(off_after > off_before, "offset should rise scrolling down");
        let y_after = tree.get_node(tree.id_at(probe)).unwrap().bounds.y;
        // Content moved up by exactly the offset delta.
        assert!(
            ((y_after - y_before) - -(off_after - off_before)).abs() < 0.01,
            "content shift {} must equal -offset-delta {}",
            y_after - y_before,
            -(off_after - off_before)
        );

        // A full rebuild at the new offset lands the same node at the same y —
        // in-place and rebuild agree, so no jump when the next rebuild lands.
        // Clear first: the live path truncates the inspector region before
        // re-minting, so two live copies of one card never coexist — and
        // card roots are identity-keyed (D4), so a no-clear double build
        // would (correctly) trip the tree's duplicate-WidgetId assert.
        tree.clear();
        panel.build(&mut tree, &layout);
        let y_rebuilt = tree
            .get_node(tree.id_at(panel.layer_chrome.first_node()))
            .unwrap()
            .bounds
            .y;
        assert!(
            (y_rebuilt - y_after).abs() < 0.01,
            "rebuild y {y_rebuilt} must match in-place y {y_after}"
        );
    }

    /// BUG-076 instrumentation: a 9-card stack (the `bug060_scene` shape —
    /// several cards with an armed, already-open audio-mod drawer, no
    /// "click to open" step) must report `layer_column_height()` equal to
    /// the sum of each card's SETTLED `compute_height()` — even on the very
    /// first `configure_layer_effects` call, with zero `tick_drawers` calls
    /// in between (the headless single-shot render path has no per-frame
    /// animation loop, per BUG-073). If `max_scroll()` were computed from an
    /// undercounted `content_height`, `try_inspector_scroll` would clamp far
    /// short of the visible overflow — the exact BUG-076 symptom.
    #[test]
    fn layer_column_height_matches_settled_heights_with_armed_audio_drawers_on_first_configure() {
        use super::super::param_card::ParamCardKind;
        use super::super::param_slider_shared::{AudioCardState, AudioRowState};

        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 500.0;
            l
        };

        // 9 cards, 4 params each — same shape as the other 9-card fixtures
        // above. Every OTHER card has its first param's audio mod armed
        // from the start (mirrors bug060_scene's mix of armed/plain cards).
        let configs: Vec<_> = (0..9)
            .map(|i| {
                let mut c = mk_config(ParamCardKind::Effect, &format!("FX{i}"), 4);
                if i % 2 == 0 {
                    c.audio = AudioCardState {
                        rows: vec![
                            AudioRowState { active: true, ..Default::default() },
                            AudioRowState::default(),
                            AudioRowState::default(),
                            AudioRowState::default(),
                        ],
                        ..Default::default()
                    };
                }
                c
            })
            .collect();

        // Single configure call — no prior "unarmed" configure, no ticks —
        // the realistic first-selection / first-render case.
        panel.configure_layer_effects(&configs, None);
        panel.configure_tabs(&[InspectorTab::Layer, InspectorTab::Master], InspectorTab::Layer);
        panel.build(&mut tree, &layout);

        let reported_height = panel.right_column_height();

        // Ticking further must not change any card's reported height — if
        // it did, `reported_height` (taken before any tick) was reading a
        // mid-tween value instead of the settled one, i.e. undercounting.
        for card in &mut panel.effects[InspectorCompositePanel::SCOPE_LAYER] {
            for _ in 0..20 {
                card.tick_drawers(20.0);
            }
        }
        let height_after_ticking = panel.right_column_height();
        assert!(
            (reported_height - height_after_ticking).abs() < 0.5,
            "right_column_height before any tick ({reported_height}) must already equal the \
             fully-settled height ({height_after_ticking}) — an armed drawer whose height only \
             appears after ticking means the first-configure value undercounted"
        );

        // The scroll bound itself must reflect real overflow, not clamp to
        // a near-zero max — this is the actual user-visible symptom.
        panel.layer_scroll.set_content_height(panel.right_column_height());
        let scrolled = panel.try_scroll_in_place(-1_000_000.0, layout.inspector().x + 10.0, &mut tree);
        assert!(scrolled, "sanity: scrollable content must exist");
        assert!(
            panel.layer_scroll.max_scroll() > 200.0,
            "max_scroll ({}) must reflect the real overflow of a 9-card, several-drawers-open \
             stack, not clamp to a handful of pixels",
            panel.layer_scroll.max_scroll()
        );
    }

    /// Dragging the scrollbar thumb now moves the content with it (in place),
    /// not just the thumb — and raises the `scrolled_in_place` signal so the app
    /// re-renders the inspector slot.
    #[test]
    fn scrollbar_drag_scrolls_content_in_place() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 400.0;
            l
        };
        let effects: Vec<_> = (0..12)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 3))
            .collect();
        panel.configure_layer_effects(&effects, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);
        assert!(panel.layer_scroll.max_scroll() > 0.0, "needs scrollable content");

        let probe = panel.layer_chrome.first_node();
        let y_before = tree.get_node(tree.id_at(probe)).unwrap().bounds.y;

        // Begin a scrollbar drag on the layer thumb, then drag toward the bottom.
        let thumb = panel.layer_scroll.thumb_id().unwrap();
        let vp = panel.layer_scroll.viewport();
        panel.route_pointer_down(thumb, Vec2::new(vp.x, vp.y), Modifiers::NONE, &tree);
        assert!(panel.dragging_scrollbar);
        let _ = panel.handle_drag(Vec2::new(vp.x, vp.y + vp.height * 0.8), &mut tree);

        assert!(
            panel.take_scrolled_in_place(),
            "scrollbar drag must raise the in-place signal"
        );
        let y_after = tree.get_node(tree.id_at(probe)).unwrap().bounds.y;
        assert!(
            y_after < y_before - 1.0,
            "content must move up when dragging the thumb down (before={y_before}, after={y_after})"
        );
        assert!(panel.layer_scroll.scroll_offset() > 0.0);
    }

    /// BUG-265: `update_card_drag`'s hit-test must track the ACTUAL
    /// on-screen card position, not the `card_y()` snapshot written only at
    /// `build()` time. Wheel/scrollbar scroll moves the tree nodes in place
    /// (`try_scroll_in_place` → `ScrollContainer::offset_content`) WITHOUT a
    /// rebuild, so `card_y()` goes stale by exactly the scroll delta while
    /// the live tree bounds (what `live_bounds()` reads) stay correct.
    fn find_drag_handle_id(card: &ParamCardPanel, tree: &UITree) -> NodeId {
        for i in card.first_node()..card.first_node() + card.node_count() {
            let id = tree.id_at(i);
            if card.is_drag_handle(id) {
                return id;
            }
        }
        panic!("card has no drag handle node in its build range");
    }

    #[test]
    fn drag_hit_test_uses_live_bounds_after_in_place_scroll() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 400.0;
            l
        };
        let effects: Vec<_> = (0..12)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 3))
            .collect();
        panel.configure_layer_effects(&effects, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);
        assert!(
            panel.layer_scroll.max_scroll() > 0.0,
            "test needs scrollable content"
        );

        // Begin a card drag (the source card is unrelated to the hit-test
        // math below — any drag handle will do).
        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][0], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));

        // Scroll in place — no rebuild. Content nodes move; `card_y()` does not.
        let cursor_x = layout.inspector().x + 10.0;
        let scrolled = panel.try_scroll_in_place(-30.0, cursor_x, &mut tree);
        assert!(scrolled, "sanity: must scroll in place");

        // The ACTUAL, post-scroll on-screen position of a card well past the
        // scroll delta — read via the same live-tree source the fix uses.
        let target_idx = 5;
        let target_bounds = panel.effects[InspectorCompositePanel::SCOPE_LAYER][target_idx]
            .live_bounds(&tree)
            .expect("built card has live bounds");
        let cursor_y = target_bounds.y + 1.0; // just inside the card's top edge

        panel.update_card_drag(Vec2::new(cursor_x, cursor_y), &mut tree);

        assert_eq!(
            panel.card_drag_target_index, target_idx,
            "drop target must match the scrolled on-screen card position, not \
             a `card_y()` snapshot stale by the scroll delta"
        );
        let indicator_bounds = tree.get_bounds(panel.card_drag_indicator_id.unwrap());
        assert!(
            (indicator_bounds.y - (target_bounds.y - DRAG_INDICATOR_H * 0.5)).abs() < 0.5,
            "indicator must be drawn at the scrolled on-screen card position: \
             got y={}, expected~{}",
            indicator_bounds.y,
            target_bounds.y - DRAG_INDICATOR_H * 0.5
        );
    }

    /// BUG-265 root cause 2: `compute_height()` re-derives from animated
    /// state (`collapse_frac()`) — mid-tween, without a rebuild, it
    /// disagrees with what's actually still painted on screen (the frozen
    /// tree bounds from the last `build()`). The fix must hit-test against
    /// the frozen tree, matching the screen, not the ticked model state.
    #[test]
    fn drag_hit_test_uses_frozen_tree_bounds_mid_animation_without_rebuild() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 400.0;
            l
        };
        let mut configs: Vec<_> = (0..6)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 3))
            .collect();
        panel.configure_layer_effects(&configs, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);

        // Snapshot the still-on-screen (frozen) position of a card below the
        // one about to collapse — this is what the cursor must still hit.
        let watch_idx = 3;
        let before_bounds = panel.effects[InspectorCompositePanel::SCOPE_LAYER][watch_idx]
            .live_bounds(&tree)
            .expect("built card has live bounds");

        // Retarget an earlier card's collapse animation, then tick it
        // PARTWAY — mid-tween, no rebuild, so the tree/screen is untouched.
        configs[1].collapsed = true;
        panel.configure_layer_effects(&configs, None);
        panel.effects[InspectorCompositePanel::SCOPE_LAYER][1].tick_drawers(20.0);
        assert!(
            panel.effects[InspectorCompositePanel::SCOPE_LAYER][1].is_collapse_animating(),
            "sanity: must actually be mid-tween, not settled"
        );

        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][0], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));

        let cursor_x = layout.inspector().x + 10.0;
        let cursor_y = before_bounds.y + 1.0;
        panel.update_card_drag(Vec2::new(cursor_x, cursor_y), &mut tree);

        assert_eq!(
            panel.card_drag_target_index, watch_idx,
            "hit-test must track the frozen, still-on-screen tree bounds — not \
             `compute_height()`, which now reads the mid-tween collapse_frac \
             and disagrees with what's actually painted until the next build()"
        );
    }

    /// Regression guard: an unscrolled, settled (no in-flight animation)
    /// layout must hit-test identically before and after the fix — the fix
    /// changes the geometry SOURCE, not the math, so plain builds are
    /// unaffected.
    #[test]
    fn drag_hit_test_matches_settled_unscrolled_layout() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 400.0;
            l
        };
        let configs: Vec<_> = (0..6)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 3))
            .collect();
        panel.configure_layer_effects(&configs, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);

        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][0], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));

        let cursor_x = layout.inspector().x + 10.0;
        for target_idx in 0..configs.len() {
            let bounds = panel.effects[InspectorCompositePanel::SCOPE_LAYER][target_idx]
                .live_bounds(&tree)
                .expect("built card has live bounds");
            let cursor_y = bounds.y + 1.0;
            panel.update_card_drag(Vec2::new(cursor_x, cursor_y), &mut tree);
            assert_eq!(
                panel.card_drag_target_index, target_idx,
                "unscrolled, settled layout: fix must agree with pre-fix \
                 behavior for card {target_idx}"
            );
        }
    }

    /// P3 geometry monopoly, case (b): the hit-test loop in
    /// `update_card_drag` sums PER-CARD live bounds, not a uniform stride —
    /// a mixed-height list (one card settled collapsed, others expanded at
    /// different row counts) must still resolve the correct drop-target
    /// index at every card boundary. This is new coverage: the W2-B
    /// (BUG-265) test family above only exercises uniform-height cards.
    #[test]
    fn drag_hit_test_target_index_with_mixed_height_cards() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();

        let mut configs = vec![
            mk_config(ParamCardKind::Effect, "FX0", 3), // expanded
            mk_config(ParamCardKind::Effect, "FX1", 3), // collapsed below
            mk_config(ParamCardKind::Effect, "FX2", 6), // expanded, taller
            mk_config(ParamCardKind::Effect, "FX3", 1), // expanded, short
        ];
        configs[1].collapsed = true;
        panel.configure_layer_effects(&configs, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);

        // Sanity: the mixed-height premise actually holds — the collapsed
        // card is shorter than its expanded neighbors, so the boundary math
        // below is exercising real height variation, not a uniform stride
        // that happens to pass.
        let collapsed_h = panel.effects[InspectorCompositePanel::SCOPE_LAYER][1]
            .live_bounds(&tree)
            .unwrap()
            .height;
        let expanded_h = panel.effects[InspectorCompositePanel::SCOPE_LAYER][0]
            .live_bounds(&tree)
            .unwrap()
            .height;
        assert!(
            collapsed_h < expanded_h,
            "test needs a real height difference: collapsed={collapsed_h} expanded={expanded_h}"
        );

        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][0], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));

        let cursor_x = layout.inspector().x + 10.0;
        for target_idx in 0..configs.len() {
            let bounds = panel.effects[InspectorCompositePanel::SCOPE_LAYER][target_idx]
                .live_bounds(&tree)
                .expect("built card has live bounds");
            let cursor_y = bounds.y + 1.0;
            panel.update_card_drag(Vec2::new(cursor_x, cursor_y), &mut tree);
            assert_eq!(
                panel.card_drag_target_index, target_idx,
                "mixed-height layout: card {target_idx} (height {}) must hit-test \
                 to its own index, not a uniform-stride guess",
                bounds.height
            );
        }
    }

    /// P3 geometry monopoly, case (d): `end_card_drag`'s target→effect-index
    /// mapping, isolated from the hit-test geometry (covered above). Exact
    /// regression pin for BUG-265 root cause 3 (findings doc): the
    /// after-last-drop branch must use the HIGHEST `effect_index` among the
    /// tab's cards, not `cards.last()`'s — this test builds a card list
    /// whose Vec order deliberately diverges from effect_index order so the
    /// two computations disagree, and pins the correct (max-based) one.
    /// Also covers the ordinary to_card < cards.len() branch with the same
    /// non-contiguous index set.
    #[test]
    fn end_card_drag_maps_target_index_to_effect_index_with_non_contiguous_indices() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();

        let mut configs: Vec<_> = (0..4)
            .map(|i| mk_config(ParamCardKind::Effect, &format!("FX{i}"), 2))
            .collect();
        // Non-monotonic effect_index, and the LAST card in Vec order (FX3)
        // is deliberately NOT the max — the divergence root cause 3 fixed.
        configs[0].effect_index = 7; // max
        configs[1].effect_index = 1; // drag source
        configs[2].effect_index = 3;
        configs[3].effect_index = 2; // last in Vec order, but not max
        panel.configure_layer_effects(&configs, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);

        // Middle drop: to_card < cards.len() reads the target card's own
        // effect_index directly.
        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][1], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));
        panel.card_drag_target_index = 2; // FX2, effect_index 3
        let actions = panel.end_card_drag(&mut tree);
        assert_eq!(actions.len(), 1, "expected one action: {actions:?}");
        match &actions[0] {
            PanelAction::Params(ParamsAction::EffectReorder(from, to)) => {
                assert_eq!(*from, 1, "dragged card's effect_index");
                assert_eq!(*to, 3, "middle drop reads the target card's own effect_index");
            }
            other => panic!("expected EffectReorder, got {other:?}"),
        }

        // After-last drop: to_card == cards.len() must use max(effect_index)
        // + 1 (7 + 1 = 8), NOT cards.last()'s effect_index + 1 (2 + 1 = 3 —
        // the pre-fix bug, and coincidentally equal to the middle-drop
        // target above, so a regression here would be easy to miss without
        // this explicit pin).
        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][1], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));
        panel.card_drag_target_index = panel.effects[InspectorCompositePanel::SCOPE_LAYER].len();
        let actions = panel.end_card_drag(&mut tree);
        assert_eq!(actions.len(), 1, "expected one action: {actions:?}");
        match &actions[0] {
            PanelAction::Params(ParamsAction::EffectReorder(from, to)) => {
                assert_eq!(*from, 1, "dragged card's effect_index");
                assert_eq!(
                    *to, 8,
                    "after-last drop must land past the HIGHEST effect_index (7), \
                     not past cards.last()'s effect_index (2)"
                );
            }
            other => panic!("expected EffectReorder, got {other:?}"),
        }
    }

    /// INV-3 regression pin (WIDGET_TREE_DESIGN §6/§7 P3): the drag
    /// interaction path reads no geometry snapshot — it follows the live
    /// tree end to end, from a post-scroll cursor position through to the
    /// emitted `PanelAction`. The `drag_hit_test_uses_live_bounds_after_in_
    /// place_scroll` test above (W2-B/BUG-265) already pins the target-index
    /// half of this; this test extends the same repro through `end_card_
    /// drag` to the dispatched command, closing the loop the invariant
    /// actually promises.
    #[test]
    fn inv3_drag_targets_follow_live_bounds_after_in_place_scroll() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = {
            let mut l = ScreenLayout::new(1920.0, 1080.0);
            l.inspector_width = 400.0;
            l
        };
        let effects: Vec<_> = (0..12)
            .map(|i| {
                let mut cfg = mk_config(ParamCardKind::Effect, &format!("FX{i}"), 3);
                // `mk_config` defaults every card's effect_index to 0 — fine
                // for the target-index-only assertions the W2-B tests make,
                // but this test asserts through to the dispatched command's
                // effect_index, so the cards need distinct, position-
                // matching indices like the real flat effects list has.
                cfg.effect_index = i;
                cfg
            })
            .collect();
        panel.configure_layer_effects(&effects, None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        panel.build(&mut tree, &layout);
        assert!(
            panel.layer_scroll.max_scroll() > 0.0,
            "test needs scrollable content"
        );

        let handle_id =
            find_drag_handle_id(&panel.effects[InspectorCompositePanel::SCOPE_LAYER][0], &tree);
        assert!(panel.try_begin_card_drag(Some(handle_id), &mut tree));

        // Scroll in place via the same API the app uses on wheel/scrollbar
        // input — no rebuild, so any snapshot geometry would go stale.
        let cursor_x = layout.inspector().x + 10.0;
        let scrolled = panel.try_scroll_in_place(-30.0, cursor_x, &mut tree);
        assert!(scrolled, "sanity: must scroll in place");

        let target_idx = 5;
        let target_bounds = panel.effects[InspectorCompositePanel::SCOPE_LAYER][target_idx]
            .live_bounds(&tree)
            .expect("built card has live bounds");
        let expected_effect_index =
            panel.effects[InspectorCompositePanel::SCOPE_LAYER][target_idx].effect_index();
        let cursor_y = target_bounds.y + 1.0;

        panel.update_card_drag(Vec2::new(cursor_x, cursor_y), &mut tree);
        assert_eq!(panel.card_drag_target_index, target_idx);

        let actions = panel.end_card_drag(&mut tree);
        assert_eq!(actions.len(), 1, "expected one action: {actions:?}");
        match &actions[0] {
            PanelAction::Params(ParamsAction::EffectReorder(_from, to)) => {
                assert_eq!(
                    *to, expected_effect_index,
                    "the dispatched command must target the effect_index of the \
                     card actually under the cursor post-scroll, not a stale \
                     geometry snapshot's idea of it"
                );
            }
            other => panic!("expected EffectReorder, got {other:?}"),
        }
    }

    #[test]
    fn find_target_for_scrollbar() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        let target =
            panel.find_target_for_node(panel.layer_scroll.track_id().unwrap());
        assert!(matches!(target, Some(PressedTarget::Scrollbar)));
    }

    #[test]
    fn find_target_for_master_chrome() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        // The master chrome's chevron button should route to MasterChrome
        let first = panel.master_chrome.first_node();
        if panel.master_chrome.node_count() > 0 {
            let target = panel.find_target_for_node(tree.id_at(first));
            assert!(matches!(target, Some(PressedTarget::MasterChrome)));
        }
    }

    #[test]
    fn click_chevron_returns_toggle() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        // Find the master chrome's chevron button and simulate click
        // We can test via route_click
        let actions = panel.route_click(
            tree.id_at(panel.master_chrome.first_node() + 1),
            Modifiers::NONE,
            &tree,
        );
        // Node at first_node+1 is the chevron button in master chrome build order
        // This should return MasterCollapseToggle
        if !actions.is_empty() {
            assert!(matches!(actions[0], PanelAction::Params(ParamsAction::MasterCollapseToggle)));
        }
    }

    #[test]
    fn is_dragging_tracks_scrollbar() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        assert!(!panel.is_dragging());

        // Simulate scrollbar pointer down.
        let sb_id = panel.layer_scroll.thumb_id().unwrap();
        let pos = Vec2::new(280.0, 100.0);
        panel.route_pointer_down(sb_id, pos, crate::input::Modifiers::NONE, &tree);

        assert!(panel.is_dragging());
        assert!(panel.dragging_scrollbar);
    }

    /// After a Master→Layer scope switch, a node in the live layer effect's range
    /// must route to that effect — never to a stale MasterEffect — because the
    /// inactive master card's range was reset to empty (Stage 2 truthfulness, and
    /// a generation-stamped id (Stage 4) wouldn't match a reused slot anyway).
    #[test]
    fn scope_switch_routes_to_active_scope_not_stale() {
        use super::super::param_card::ParamCardKind;
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();

        // Master active, one master effect.
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "MasterFX", 2)]);
        panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        tree.clear();
        panel.build(&mut tree, &layout);
        assert!(panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].node_count() > 0);

        // Switch to Layer with a layer effect; the master effect is not built.
        panel.configure_layer_effects(&[mk_config(ParamCardKind::Effect, "LayerFX", 2)], None);
        panel.configure_tabs(
            &[InspectorTab::Layer, InspectorTab::Master],
            InspectorTab::Layer,
        );
        tree.clear();
        panel.build(&mut tree, &layout);

        // The inactive master effect reports not-built (empty range)…
        assert_eq!(panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].node_count(), 0);
        // …and a node in the live layer effect routes to LayerEffect.
        let lc = &panel.effects[InspectorCompositePanel::SCOPE_LAYER][0];
        assert!(lc.node_count() > 0);
        let probe = lc.first_node();
        let target = panel.find_target_for_node(tree.id_at(probe));
        assert!(
            matches!(target, Some(PressedTarget::LayerEffect(0))),
            "live layer node must route to LayerEffect, got {target:?}"
        );
    }

    /// The macro bank is a global control, so it pins to the very top of the
    /// inspector — above the per-scope tab strip and the scrollable columns.
    #[test]
    fn macros_panel_sits_above_the_tab_strip() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        panel.build(&mut tree, &layout);

        let insp = layout.inspector();
        let macros_first = panel.macros_panel().first_node();
        assert_ne!(macros_first, usize::MAX, "macros always builds");
        let macros_y = tree.get_bounds(tree.id_at(macros_first)).y;

        // Pinned to the inspector top, and above where the columns (and the tab
        // strip between) begin.
        assert!(
            macros_y <= insp.y + 2.0,
            "macros pinned to inspector top: {macros_y} vs {}",
            insp.y
        );
        assert!(
            panel.columns_y > macros_y,
            "columns/tabs sit below the macros strip: columns_y={} macros_y={macros_y}",
            panel.columns_y
        );
    }

    // ── D17 spawn pop / delete collapse (reconcile_cards) ──────────────

    #[test]
    fn spawn_pop_fires_for_a_new_card_not_a_reused_one() {
        use super::super::param_card::ParamCardKind;
        let mut panel = InspectorCompositePanel::new();
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "A", 2)]);
        assert!(
            panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].is_spawning(),
            "a genuinely new card pops in"
        );

        // Settle it, then reconfigure with the SAME effect identity — reused,
        // must not re-pop just because the panel rebuilt.
        for _ in 0..20 {
            panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].tick_drawers(20.0);
        }
        assert!(
            !panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].is_spawning(),
            "settled"
        );
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "A", 2)]);
        assert!(
            !panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].is_spawning(),
            "a reused card never re-pops on reconfigure"
        );
    }

    #[test]
    fn removed_effect_moves_to_dying_and_collapses_instead_of_vanishing() {
        use super::super::param_card::ParamCardKind;
        let mut panel = InspectorCompositePanel::new();
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "A", 2)]);
        // Settle the spawn-pop so it doesn't interfere with reading collapse state.
        for _ in 0..20 {
            panel.effects[InspectorCompositePanel::SCOPE_MASTER][0].tick_drawers(20.0);
        }
        assert!(panel.master_dying.is_empty());

        // Reconfigure with an EMPTY config list — "A" was removed from the model.
        panel.configure_master_effects(&[]);
        assert!(
            panel.effects[InspectorCompositePanel::SCOPE_MASTER].is_empty(),
            "no longer a live card"
        );
        assert_eq!(panel.master_dying.len(), 1, "removed card moves to the exit-state list");
        assert!(panel.master_dying[0].is_collapse_animating(), "starts collapsing");
        assert!(!panel.master_dying[0].is_delete_finished(), "not finished the instant it dies");

        // Run the exit animation to completion.
        for _ in 0..30 {
            panel.master_dying[0].tick_drawers(20.0);
        }
        assert!(panel.master_dying[0].is_delete_finished(), "exit animation completes");
    }

    #[test]
    fn switching_layers_drops_old_cards_instantly_without_the_delete_collapse() {
        use super::super::param_card::ParamCardKind;
        let layer_a = manifold_foundation::LayerId::new("layer-a");
        let layer_b = manifold_foundation::LayerId::new("layer-b");

        let mut panel = InspectorCompositePanel::new();
        // Layer A: a two-effect chain, settled.
        panel.configure_layer_effects(
            &[
                mk_config(ParamCardKind::Effect, "A1", 2),
                mk_config(ParamCardKind::Effect, "A2", 2),
            ],
            Some(&layer_a),
        );
        for _ in 0..20 {
            for c in &mut panel.effects[InspectorCompositePanel::SCOPE_LAYER] {
                c.tick_drawers(20.0);
            }
        }
        assert_eq!(panel.effects[InspectorCompositePanel::SCOPE_LAYER].len(), 2);
        assert!(panel.layer_dying.is_empty());

        // Navigate to layer B (a different scope, a different chain). Layer A's
        // effects were NOT removed from the model — just navigated away from —
        // so they must vanish instantly, never entering the delete-collapse
        // `dying` list (which is what left the stale chain lingering over the
        // new selection for a few frames).
        panel.configure_layer_effects(
            &[mk_config(ParamCardKind::Effect, "B1", 2)],
            Some(&layer_b),
        );
        assert_eq!(
            panel.effects[InspectorCompositePanel::SCOPE_LAYER].len(),
            1,
            "now showing layer B's chain"
        );
        assert!(
            panel.layer_dying.is_empty(),
            "a layer switch drops the old cards instantly — no stale collapse"
        );

        // Contrast: a same-scope removal (still layer B) DOES collapse — the
        // exit animation is reserved for genuine in-place deletions.
        panel.configure_layer_effects(&[], Some(&layer_b));
        assert_eq!(
            panel.layer_dying.len(),
            1,
            "removing an effect in-place still routes through the collapse"
        );
    }

    /// BUG-060 (stale fragments at the scroll-viewport edges): every node of
    /// every effect card must render UNDER the scroll column's clip. Renders a
    /// card's sub-region exactly the way the cache manager's incremental path
    /// does — `traverse_flat_range` with its pre-pushed ancestor clips,
    /// intersecting on push like `UIRenderer::handle_push_clip` — and asserts
    /// the effective clip at every background-filled node is bounded by the
    /// scroll viewport. A node whose effective clip reaches past the viewport
    /// (or that draws with no clip at all) paints into the tab-strip band;
    /// its abandoned copies after each scroll step are exactly the live-rig
    /// artifact (the escaped ON pill measured in the 2026-07-10 atlas dump).
    #[test]
    fn bug060_every_card_node_renders_under_the_column_clip() {
        use crate::tree::TraversalEvent;
        use crate::ParamCardKind;

        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        panel.configure_tabs(&[InspectorTab::Master], InspectorTab::Master);
        panel.configure_master_effects(&[mk_config(ParamCardKind::Effect, "EdgeStretch", 3)]);
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        let viewport = panel.master_scroll.viewport();
        assert!(viewport.height > 0.0, "sanity: master column viewport exists");
        let card = &panel.effects[InspectorCompositePanel::SCOPE_MASTER][0];
        let (start, end) = (card.first_node(), card.first_node() + card.node_count());
        assert!(end > start, "sanity: card built nodes");

        fn intersect(a: Rect, b: Rect) -> Rect {
            let x = a.x.max(b.x);
            let y = a.y.max(b.y);
            let x2 = (a.x + a.width).min(b.x + b.width);
            let y2 = (a.y + a.height).min(b.y + b.height);
            Rect::new(x, y, (x2 - x).max(0.0), (y2 - y).max(0.0))
        }

        let mut clip_stack: Vec<Rect> = Vec::new();
        let mut violations: Vec<String> = Vec::new();
        let mut checked = 0usize;
        tree.traverse_flat_range(start, end, false, |ev| match ev {
            TraversalEvent::PushClip(r) => {
                let clipped = clip_stack.last().map_or(r, |c| intersect(*c, r));
                clip_stack.push(clipped);
            }
            TraversalEvent::PopClip => {
                clip_stack.pop();
            }
            TraversalEvent::Node(node) => {
                // Only nodes that draw an opaque fill can deposit artifact
                // pixels — labels and transparent buttons paint no background.
                if node.style.bg_color.a == 0 {
                    return;
                }
                checked += 1;
                let ok = clip_stack.last().is_some_and(|c| {
                    c.y >= viewport.y - 0.5
                        && c.y + c.height <= viewport.y + viewport.height + 0.5
                });
                if !ok {
                    violations.push(format!(
                        "node {:?} text={:?} bounds={:?} effective_clip={:?} (viewport {:?})",
                        node.id,
                        node.text,
                        node.bounds,
                        clip_stack.last(),
                        viewport,
                    ));
                }
            }
        });
        assert!(checked > 0, "sanity: card produced background-filled nodes");
        assert!(
            violations.is_empty(),
            "{} card node(s) render without a viewport-bounded clip:\n{}",
            violations.len(),
            violations.join("\n")
        );
    }
}
