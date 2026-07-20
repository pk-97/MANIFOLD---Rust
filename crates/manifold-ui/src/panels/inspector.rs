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

    /// Set which tab rungs are available (display order, local→global) and which
    /// is active. Drives section visibility so only the active scope renders.
    pub fn configure_tabs(&mut self, available: &[InspectorTab], active: InspectorTab) {
        self.available_tabs.clear();
        self.available_tabs.extend_from_slice(available);
        self.set_active_tab(active);
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

    /// Build the tab strip: one button per available rung, the active one
    /// highlighted. Records node ids for click routing.
    fn build_tab_strip(&mut self, tree: &mut UITree, rect: Rect) {
        self.tab_node_ids.clear();
        self.collapse_all_btn_id = None;
        self.compact_toggle_btn_id = None;
        if self.available_tabs.is_empty() {
            return;
        }
        let n = self.available_tabs.len();
        // The per-column controls (compact toggle + collapse-all) are anchored to
        // the strip's RIGHT EDGE — a fixed position regardless of which tab is
        // active, so they don't slide when selection changes. The tabs lay out in
        // the remaining width on the left. The block claims `controls_extra` at the
        // right: [gap][cog][gap][collapse]. Hidden when the active column has no
        // cards (then the tabs span the full width).
        let show_controls = self.active_column_card_count() > 0;
        let controls_extra = if show_controls {
            TAB_GAP + COMPACT_TOGGLE_W + TAB_GAP + COLLAPSE_ALL_W
        } else {
            0.0
        };
        let tab_area_w = (rect.width - controls_extra).max(1.0);
        let inter_gap = TAB_GAP * n.saturating_sub(1) as f32;
        let tab_w = ((tab_area_w - inter_gap) / n as f32).floor().max(1.0);

        let tabs = self.available_tabs.clone();
        let mut x = rect.x;
        for (idx, tab) in tabs.iter().enumerate() {
            if idx > 0 {
                x += TAB_GAP;
            }
            let active = *tab == self.active_tab;
            // The kit segmented-control cell — the Clip/Layer/Master tabs and any
            // other tab strip share one look.
            let mut style = UIStyle {
                font_size: TAB_FONT_SIZE,
                ..chrome::components::segment_style(active)
            };
            // Tint the SELECTED tab toward the one inspector accent (not a lane
            // hue), so the active scope reads as selected without per-layer colour.
            if active {
                style.bg_color = color::mix(color::BG_2, color::INSPECTOR_ACCENT, 0.30);
            }
            // Interactive button (not a label) so clicks hit-test and route —
            // a plain label carries no INTERACTIVE flag and is invisible to the
            // event system, which is why the tabs were unclickable.
            let id = tree.add_button(None, x, rect.y, tab_w, rect.height, style, Self::tab_label(*tab));
            self.tab_node_ids.push((id, *tab));
            x += tab_w;
        }

        // Controls anchored to the strip's right edge — fixed, independent of the
        // active tab. cog_x is back-computed so the collapse button's right edge
        // lands flush at `rect.right`.
        if show_controls {
            let cog_x = rect.x + rect.width - COLLAPSE_ALL_W - TAB_GAP - COMPACT_TOGGLE_W;
            self.build_tab_controls(tree, cog_x, rect.y, rect.height);
        }
    }

    /// Build the active tab's per-column controls (compact toggle + collapse-all),
    /// laid out left→right starting at `x`. Returns the x after the last control.
    /// They act on the active tab's column (the single source of truth).
    fn build_tab_controls(&mut self, tree: &mut UITree, x: f32, y: f32, h: f32) -> f32 {
        // §6b — compact toggle (cog): hide every card's modulation config drawers
        // while keeping mods armed. The kit toggle — accent fill when engaged.
        let id = tree.add_button(
            None,
            x,
            y,
            COMPACT_TOGGLE_W,
            h,
            UIStyle {
                font_size: color::FONT_BODY,
                ..chrome::components::toggle_style(self.mods_compact)
            },
            // cog (atlas icon) — hide/show modulation settings
            &crate::icons::Icon::Cog.text(),
        );
        self.compact_toggle_btn_id = Some(id);

        // Collapse-all / expand-all. Label reflects the action it will take:
        // "Collapse" while any card is open, else "Expand".
        let x = x + COMPACT_TOGGLE_W + TAB_GAP;
        let any_expanded = self.any_active_card_expanded();
        let id = tree.add_button(
            None,
            x,
            y,
            COLLAPSE_ALL_W,
            h,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: color::FONT_BODY,
                ..chrome::components::button_secondary_style()
            },
            if any_expanded { "Collapse" } else { "Expand" },
        );
        self.collapse_all_btn_id = Some(id);
        x + COLLAPSE_ALL_W
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

    /// Sub-region node ranges for incremental cache re-rendering.
    /// Returns (node_start, node_end) for each sub-panel: chrome panels,
    /// effect cards, gen params. Used by the cache manager to detect which
    /// parts of the inspector changed and only re-render those.
    ///
    /// Every sub-panel is offered, but only the ones that built nodes this frame
    /// contribute a range: an inactive scope was reset to an empty range in
    /// `build`, and `push` drops `(usize::MAX, 0)` — so the cache is fed exactly
    /// what was actually built. No `*_visible()` gate; the node range is the
    /// single source of truth for "live this frame".
    pub fn sub_region_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges = Vec::with_capacity(
            4 + self.effects[Self::SCOPE_MASTER].len() + self.effects[Self::SCOPE_LAYER].len() + 1,
        );
        let push = |ranges: &mut Vec<(usize, usize)>, first: usize, count: usize| {
            if first != usize::MAX && count > 0 {
                ranges.push((first, first + count));
            }
        };
        push(
            &mut ranges,
            self.macros_panel.first_node(),
            self.macros_panel.node_count(),
        );
        push(
            &mut ranges,
            self.master_chrome.first_node(),
            self.master_chrome.node_count(),
        );
        for card in &self.effects[Self::SCOPE_MASTER] {
            push(&mut ranges, card.first_node(), card.node_count());
        }
        push(
            &mut ranges,
            self.layer_chrome.first_node(),
            self.layer_chrome.node_count(),
        );
        if let Some(ref gp) = self.gen_params {
            push(&mut ranges, gp.first_node(), gp.node_count());
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            push(&mut ranges, card.first_node(), card.node_count());
        }
        push(
            &mut ranges,
            self.clip_chrome.first_node(),
            self.clip_chrome.node_count(),
        );
        ranges
    }

    pub fn configure_master_effects(&mut self, configs: &[ParamSurface]) {
        let existing = std::mem::take(&mut self.effects[Self::SCOPE_MASTER]);
        self.effects[Self::SCOPE_MASTER] =
            Self::reconcile_cards(existing, configs, &mut self.master_dying, self.card_context);
    }

    pub fn configure_layer_effects(&mut self, configs: &[ParamSurface], scope: Option<&LayerId>) {
        // A change of scope is navigation, not an edit of the current chain:
        // the previously-shown layer's effects weren't removed from the model,
        // so they must not play the delete-collapse exit animation.
        // `reconcile_cards` can't tell the difference — on a switch none of the
        // old cards match the new layer's effect IDs, so it would move every
        // one of them into `layer_dying` and the whole stale chain would linger
        // mid-collapse over the new selection. Drop them instantly instead, and
        // abandon any in-flight death carried over from the old scope.
        if scope != self.layer_scope_id.as_ref() {
            self.effects[Self::SCOPE_LAYER].clear();
            self.layer_dying.clear();
            self.layer_scope_id = scope.cloned();
        }
        let existing = std::mem::take(&mut self.effects[Self::SCOPE_LAYER]);
        self.effects[Self::SCOPE_LAYER] =
            Self::reconcile_cards(existing, configs, &mut self.layer_dying, self.card_context);
    }

    pub fn configure_gen_params(
        &mut self,
        config: Option<&ParamSurface>,
        layer_id: Option<LayerId>,
    ) {
        // The generator card is a single optional, distinct from the effect
        // lists (it carries no EffectId and is outside the selection +
        // drag-reorder model). Reuse the existing panel when the selection still
        // points at the same layer's generator, so its transient UI state (the
        // modulation config tab) survives the rebuild. `set_layer_id` is applied
        // before `configure` per its contract.
        self.gen_params = config.map(|cfg| {
            let reused = self
                .gen_params
                .take()
                .filter(|p| p.owning_layer_id() == layer_id.as_ref());
            let mut panel = reused.unwrap_or_default();
            panel.set_context(self.card_context);
            panel.set_layer_id(layer_id);
            panel.configure(cfg);
            panel
        });
    }

    /// Set the chrome context applied to every card this panel owns —
    /// `CardContext::Author` for the graph-editor window's inspector
    /// instance, `CardContext::Perform` (the default) for the main window's.
    /// Set once by the host at construction (mirrors
    /// `ParamCardPanel::set_context`'s doc comment); applies immediately to
    /// every card currently held (including dying ones, so an in-flight
    /// collapse doesn't flash back to Perform chrome) and every card built
    /// afterward via `reconcile_cards` / `configure_gen_params`.
    pub fn set_card_context(&mut self, context: CardContext) {
        self.card_context = context;
        for card in self
            .effects
            .iter_mut()
            .flatten()
            .chain(self.gen_params.iter_mut())
            .chain(self.master_dying.iter_mut())
            .chain(self.layer_dying.iter_mut())
        {
            card.set_context(context);
        }
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

    /// Reconcile the existing card panels against the new configs, **reusing** a
    /// panel whose effect identity matches so its transient UI-only state (the
    /// modulation config tab, drag, copy-flash) survives the per-snapshot
    /// rebuild instead of being thrown away. The result is in config (effect)
    /// order.
    ///
    /// D17 additions on top of the original reuse mechanism: a config with NO
    /// matching existing panel is genuinely new — its fresh panel fires the
    /// "spawn pop" scale-in (`ParamCardPanel::fire_spawn_pop`). An existing
    /// panel with no matching config anymore was just removed from the model;
    /// instead of dropping it here, it moves into `dying` — the exit-state
    /// pattern (`anim.rs`'s doc comment) — so the caller keeps drawing it
    /// through its collapse+fade instead of it vanishing mid-frame.
    ///
    /// Replaces the old build-fresh-every-frame path, which reset transient UI
    /// state every sync and re-allocated every panel each frame.
    fn reconcile_cards(
        mut existing: Vec<ParamCardPanel>,
        configs: &[ParamSurface],
        dying: &mut Vec<ParamCardPanel>,
        card_context: CardContext,
    ) -> Vec<ParamCardPanel> {
        let reconciled = configs
            .iter()
            .map(|cfg| match existing.iter().position(|c| c.matches_effect_config(cfg)) {
                Some(pos) => {
                    let mut card = existing.remove(pos);
                    card.configure(cfg);
                    card
                }
                None => {
                    let mut card = ParamCardPanel::default();
                    card.set_context(card_context);
                    card.configure(cfg);
                    card.fire_spawn_pop();
                    card
                }
            })
            .collect();
        // Whatever's left in `existing` no longer matches any config — it was
        // removed from the model this rebuild. Keep it alive in `dying`
        // rather than letting it drop here.
        for mut card in existing {
            card.begin_delete_collapse();
            dying.push(card);
        }
        reconciled
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

    /// Call on mouse wheel within the inspector viewport.
    /// Positive delta scrolls down.
    pub fn handle_scroll(&mut self, delta: f32) {
        self.handle_scroll_at(delta, self.viewport_rect.x + self.viewport_rect.width * 0.5);
    }

    pub fn handle_scroll_at(&mut self, delta: f32, cursor_x: f32) {
        if cursor_x < self.column_split_x {
            self.master_scroll.apply_scroll_delta(delta);
        } else {
            self.layer_scroll.apply_scroll_delta(delta);
        }
    }

    /// Scroll the inspector in place — the cheap path that mirrors how the
    /// timeline viewport scrolls. Applies the delta to the column under the
    /// cursor, then offsets that column's already-built content nodes instead of
    /// triggering a full `ui_root.build()` + whole-atlas clear. The caller
    /// invalidates only the inspector's cache slot afterwards.
    ///
    /// Returns `false` only when there is nothing built to offset yet (the very
    /// first frame), in which case it has NOT touched the scroll offset and the
    /// caller must fall back to `handle_scroll_at` + a rebuild. Once built it
    /// always handles the scroll in place (returning `true`), so the two paths
    /// never both apply the delta.
    pub fn try_scroll_in_place(&mut self, delta: f32, cursor_x: f32, tree: &mut UITree) -> bool {
        if self.bg_panel_id.is_none() {
            return false;
        }
        let moved = {
            let scroll = if cursor_x < self.column_split_x {
                &mut self.master_scroll
            } else {
                &mut self.layer_scroll
            };
            let old = scroll.scroll_offset();
            if !scroll.apply_scroll_delta(delta) {
                // Already at a scroll limit — consumed, nothing moved.
                return true;
            }
            let delta_y = -(scroll.scroll_offset() - old);
            let moved = scroll.offset_content(tree, delta_y);
            if moved {
                scroll.update_scrollbar(tree);
            }
            moved
        };
        if moved {
            self.scrolled_in_place = true;
        }
        true
    }

    /// Drain the "scrolled in place this frame" signal. The app calls this once
    /// per frame and invalidates the inspector's cache slot when it returns true.
    pub fn take_scrolled_in_place(&mut self) -> bool {
        std::mem::take(&mut self.scrolled_in_place)
    }

    /// Content height for the master column (left).
    fn master_column_height(&self) -> f32 {
        if !self.master_visible() {
            return 0.0;
        }
        let mut h = SECTION_CARD_PAD + self.master_chrome.compute_height();
        if !self.master_chrome.is_collapsed() {
            for card in &self.effects[Self::SCOPE_MASTER] {
                h += card.compute_height() + SECTION_GAP;
            }
            h += ADD_EFFECT_BTN_H + SECTION_GAP;
        }
        h + SECTION_CARD_PAD
    }

    /// Content height for the layer column (right).
    /// Order: layer chrome → AUDIO TRIGGERS (P3b) → gen params → layer
    /// effects → add effect button.
    fn layer_column_height(&self) -> f32 {
        let mut h = 0.0;
        if self.layer_visible() {
            h += SECTION_CARD_PAD + self.layer_chrome.compute_height();
            if !self.layer_chrome.is_collapsed() {
                // AUDIO TRIGGERS sits at the top of the layer's detail
                // content — above gen params and layer effects.
                h += self.audio_trigger_section.height() + SECTION_GAP;
                // Gen params sit above layer effects
                if let Some(ref gp) = self.gen_params {
                    h += gp.compute_height() + SECTION_GAP;
                }
                for card in &self.effects[Self::SCOPE_LAYER] {
                    h += card.compute_height() + SECTION_GAP;
                }
                h += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            h += SECTION_CARD_PAD + SECTION_GAP;
        }
        h
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

    /// Total scrollable content height for the right (Layer + Clip) column.
    fn right_column_height(&self) -> f32 {
        self.layer_column_height() + self.clip_section_height()
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

    /// Get the selection set and cards vec for a given tab.
    fn selection_for_tab(&self, tab: InspectorTab) -> (&HashSet<EffectId>, &[ParamCardPanel]) {
        let set = match tab {
            InspectorTab::Master => &self.selected_master_ids,
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => &self.selected_layer_ids,
        };
        (set, self.cards_for_tab(tab))
    }

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

    fn selection_set_mut(&mut self, tab: InspectorTab) -> &mut HashSet<EffectId> {
        match tab {
            InspectorTab::Master => &mut self.selected_master_ids,
            InspectorTab::Layer | InspectorTab::Group | InspectorTab::Clip => &mut self.selected_layer_ids,
        }
    }

    /// Unity EffectSelectionManager.OnCardClicked (lines 164-177)
    /// Dispatches to select/toggle/range based on modifiers.
    fn on_effect_card_clicked(
        &mut self,
        tab: InspectorTab,
        card_index: usize,
        modifiers: Modifiers,
    ) {
        let cmd = modifiers.ctrl || modifiers.command;
        let shift = modifiers.shift;

        if shift {
            self.range_select_effects(tab, card_index);
        } else if cmd {
            self.toggle_effect_selection(tab, card_index);
        } else {
            self.select_effect(tab, card_index);
        }
    }

    /// Unity EffectSelectionManager.SelectCard (lines 89-100)
    /// Select a single card, clearing all others across ALL tabs.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn select_effect(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() {
            return;
        }
        let id = cards[card_index].effect_id().clone();

        // Clear all tabs so only one card is selected globally
        self.selected_master_ids.clear();
        self.selected_layer_ids.clear();

        let set = self.selection_set_mut(tab);
        set.insert(id.clone());
        self.set_last_clicked_for_tab(tab, Some(id));
    }

    /// Unity EffectSelectionManager.ToggleCardSelection (lines 103-118)
    /// Cmd+Click: toggle in/out of multi-selection.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn toggle_effect_selection(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() {
            return;
        }
        let id = cards[card_index].effect_id().clone();

        let set = self.selection_set_mut(tab);
        if set.contains(&id) {
            set.remove(&id);
        } else {
            set.insert(id.clone());
        }
        self.set_last_clicked_for_tab(tab, Some(id));
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

    /// Select all effects in the active tab.
    /// Returns true if any effects were selected.
    pub fn select_all_effects(&mut self) -> bool {
        let tab = self.last_effect_tab;
        let cards = self.cards_for_tab(tab);
        if cards.is_empty() {
            return false;
        }
        let ids: Vec<EffectId> = cards.iter().map(|c| c.effect_id().clone()).collect();
        let first_id = ids[0].clone();

        let set = self.selection_set_mut(tab);
        set.clear();
        for id in ids {
            set.insert(id);
        }
        self.set_last_clicked_for_tab(tab, Some(first_id));
        true
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

    /// How many effects are selected in the active tab.
    pub fn selected_effect_count(&self) -> usize {
        let (set, _) = self.selection_for_tab(self.last_effect_tab);
        set.len()
    }

    // ── Node range ownership ─────────────────────────────────────

    /// Resolve a double-clicked node to a numeric param's value cell across the
    /// visible cards and return its type-in action (empty if it isn't a value
    /// cell). Enum/toggle params are filtered out by the card itself.
    fn route_value_typein(&self, node_id: NodeId, tree: &UITree) -> Vec<PanelAction> {
        // No scope gate: a card that didn't build this frame is not live and its
        // `value_cell_typein` returns None, so only the active scope's cards can
        // match. The card's liveness is the single source of truth.
        for card in &self.effects[Self::SCOPE_MASTER] {
            if let Some(a) = card.value_cell_typein(node_id, tree) {
                return vec![a];
            }
        }
        if let Some(gp) = self.gen_params.as_ref()
            && let Some(a) = gp.value_cell_typein(node_id, tree)
        {
            return vec![a];
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            if let Some(a) = card.value_cell_typein(node_id, tree) {
                return vec![a];
            }
        }
        Vec::new()
    }

    /// Resolve a clicked node to a driver Free-period field across the visible
    /// cards and return its type-in action (empty if it isn't a Free field).
    fn route_driver_period_typein(&self, node_id: NodeId, tree: &UITree) -> Vec<PanelAction> {
        // No scope gate — a non-live card's `driver_period_typein` returns None
        // (see `route_value_typein`).
        for card in &self.effects[Self::SCOPE_MASTER] {
            if let Some(a) = card.driver_period_typein(node_id, tree) {
                return vec![a];
            }
        }
        if let Some(gp) = self.gen_params.as_ref()
            && let Some(a) = gp.driver_period_typein(node_id, tree)
        {
            return vec![a];
        }
        for card in &self.effects[Self::SCOPE_LAYER] {
            if let Some(a) = card.driver_period_typein(node_id, tree) {
                return vec![a];
            }
        }
        Vec::new()
    }

    fn find_target_for_node(&self, node_id: NodeId) -> Option<PressedTarget> {
        let idx = node_id.index();
        // Macros panel (above both columns)
        if self.macros_panel.owns_node(node_id) {
            return Some(PressedTarget::Macros);
        }
        // AUDIO TRIGGERS section (top of the layer column's content)
        if self.audio_trigger_section.owns_node(node_id) {
            return Some(PressedTarget::AudioTriggers);
        }

        // Scrollbars
        if Some(node_id) == self.master_scroll.track_id()
            || Some(node_id) == self.master_scroll.thumb_id()
            || Some(node_id) == self.layer_scroll.track_id()
            || Some(node_id) == self.layer_scroll.thumb_id()
        {
            return Some(PressedTarget::Scrollbar);
        }

        // Every section below is matched purely by its node range. Only the
        // active scope built nodes this frame; the rest were reset to empty
        // ranges in `build`, and `in_range` is false for an empty range — so no
        // `*_visible()` gate is needed and an inactive scope can't match a live
        // index. The node range is the single source of truth.

        // Master section
        if in_range(
            idx,
            self.master_chrome.first_node(),
            self.master_chrome.node_count(),
        ) {
            return Some(PressedTarget::MasterChrome);
        }
        for (i, card) in self.effects[Self::SCOPE_MASTER].iter().enumerate() {
            if in_range(idx, card.first_node(), card.node_count()) {
                return Some(PressedTarget::MasterEffect(i));
            }
        }

        // Layer section. The generator card lives here (built and range-registered
        // alongside the layer chrome), so it is hit-tested here — not under the
        // clip section, which is a different scope.
        if in_range(
            idx,
            self.layer_chrome.first_node(),
            self.layer_chrome.node_count(),
        ) {
            return Some(PressedTarget::LayerChrome);
        }
        if let Some(ref gp) = self.gen_params
            && in_range(idx, gp.first_node(), gp.node_count())
        {
            return Some(PressedTarget::GenParam);
        }
        for (i, card) in self.effects[Self::SCOPE_LAYER].iter().enumerate() {
            if in_range(idx, card.first_node(), card.node_count()) {
                return Some(PressedTarget::LayerEffect(i));
            }
        }

        // Clip section
        if in_range(
            idx,
            self.clip_chrome.first_node(),
            self.clip_chrome.node_count(),
        ) {
            return Some(PressedTarget::ClipChrome);
        }

        None
    }

    // ── Drag event routing (needs &mut UITree) ───────────────────

    /// Whether a sub-panel is currently pressed (active drag target).
    pub fn has_pressed_target(&self) -> bool {
        self.pressed_target.is_some() || self.dragging_scrollbar || self.card_drag_active
    }

    /// Whether an effect card reorder drag is in progress.
    pub fn is_card_drag_active(&self) -> bool {
        self.card_drag_active
    }

    /// First node ID of the drag ghost/indicator overlay (for render pass).
    /// Returns None if no drag is active. Reports the wrapping `Ghost`-tier
    /// region's root (not `card_drag_ghost_id`, the label node itself) —
    /// the render pass's `render_tree_range(start, usize::MAX)` walks
    /// registered regions, and the region root sits one index before the
    /// label, so reporting the label's own index would make that walk miss
    /// the region entirely.
    pub fn card_drag_first_node(&self) -> Option<usize> {
        if self.card_drag_active {
            self.card_drag_region_root.map(|id| id.index())
        } else {
            None
        }
    }

    /// Route drag events to the pressed sub-panel.
    /// Called from UIRoot::process_events (not through Panel::handle_event)
    /// because it needs &mut UITree for slider visual feedback.
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_scrollbar {
            // Drag the thumb to an absolute offset, then offset the content nodes
            // by the delta — the same in-place scroll the wheel uses. Previously
            // only the thumb moved (the content stayed frozen until some later
            // rebuild), because the drag carried no rebuild trigger.
            let scroll = if self.dragging_scrollbar_master {
                &mut self.master_scroll
            } else {
                &mut self.layer_scroll
            };
            let old = scroll.scroll_offset();
            scroll.drag_to_scroll(pos.y);
            let delta_y = -(scroll.scroll_offset() - old);
            let moved = scroll.offset_content(tree, delta_y);
            scroll.update_scrollbar(tree);
            if moved {
                self.scrolled_in_place = true;
            }
            return vec![PanelAction::InspectorScrolled(0.0)];
        }

        if let Some(target) = self.pressed_target {
            match target {
                PressedTarget::Macros => self.macros_panel.handle_drag(pos.x, tree),
                PressedTarget::AudioTriggers => self.audio_trigger_section.handle_drag(pos.x, tree),
                PressedTarget::MasterChrome => self.master_chrome.handle_drag(pos, tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag(pos, tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag(pos, tree),
                PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_drag(pos, tree))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    /// Route drag-end events to the pressed sub-panel.
    /// Call directly from the app layer (not through Panel::handle_event).
    pub fn handle_drag_end(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_scrollbar {
            self.dragging_scrollbar = false;
            self.pressed_target = None;
            return Vec::new();
        }

        let actions = if let Some(target) = self.pressed_target {
            match target {
                PressedTarget::Macros => self.macros_panel.handle_release(),
                PressedTarget::AudioTriggers => self.audio_trigger_section.handle_release(),
                PressedTarget::MasterChrome => self.master_chrome.handle_drag_end(tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag_end(tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag_end(tree),
                PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                    .get_mut(i)
                    .map(|c| c.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        };

        self.pressed_target = None;
        actions
    }

    // ── Effect card drag-reorder (Unity EffectsListBitmapPanel) ──

    /// Try to begin a card drag on a DragBegin event. Returns true if drag started.
    /// Called from ui_root.rs on DragBegin (needs &mut UITree). `node_id` is
    /// `Option` (D9, `docs/DRAG_CAPTURE_DESIGN.md`) — a `None` means the
    /// pressed node died before the drag threshold crossed, so no card drag
    /// can be identified; that's a no-op here, same as a `Some` id matching
    /// no drag handle.
    pub fn try_begin_card_drag(&mut self, node_id: Option<NodeId>, tree: &mut UITree) -> bool {
        let Some(node_id) = node_id else {
            return false;
        };
        // Check each tab's effect cards for a drag handle match
        if let Some((tab, card_idx, fx_idx, name)) = self.find_drag_handle(node_id) {
            self.card_drag_active = true;
            self.card_drag_tab = tab;
            self.card_drag_source_index = card_idx;
            self.card_drag_effect_index = fx_idx;
            self.card_drag_target_index = card_idx;
            self.card_drag_label = name;
            self.last_effect_tab = tab;

            // Dim source card(s) border (Unity: SetDragDimmed(true))
            // If dragged card is part of a multi-selection, dim all selected
            let dragged_id = self
                .cards_for_tab(tab)
                .get(card_idx)
                .map(|c| c.effect_id().clone());
            let sel = self.selection_set_mut(tab);
            let is_multi = dragged_id
                .as_ref()
                .is_some_and(|id| sel.len() > 1 && sel.contains(id));
            if is_multi {
                let sel_ids: HashSet<EffectId> = sel.clone();
                let cards = self.cards_for_tab(tab);
                for card in cards {
                    if sel_ids.contains(card.effect_id()) {
                        card.set_drag_dimmed(tree, true);
                    }
                }
            } else {
                let cards = self.cards_for_tab(tab);
                if let Some(card) = cards.get(card_idx) {
                    card.set_drag_dimmed(tree, true);
                }
            }

            // Create ghost + indicator nodes — scoped to the correct column
            // Single full-width active column — both tabs drag within it.
            //
            // Ghost tier + ALLOW_OVERFLOW (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md`
            // D1–D3): the ghost tracks the cursor for the drag's whole
            // lifetime (`update_card_drag` below), so it must be able to
            // paint outside whatever rect it starts at and outside the
            // inspector's own region clip — the region mechanism's one
            // sanctioned overflow case. `try_begin_card_drag` runs from an
            // input event, not the panel's own `build()`, so this region is
            // minted fresh per drag (there is no already-open region to
            // nest under at this point) and torn down in `end_card_drag`.
            let col_x = self.viewport_rect.x + COLUMN_PAD;
            let col_w = (self.viewport_rect.width - COLUMN_PAD * 2.0).max(0.0);
            let ghost_w = (col_w - 24.0).min(160.0);
            let region = tree.begin_region(
                self.viewport_rect,
                ZTier::Ghost,
                "card_drag_ghost",
                UIFlags::ALLOW_OVERFLOW,
            );
            let region_start = tree.count();
            self.card_drag_ghost_id = Some(tree.add_label(
                None,
                0.0,
                -100.0,
                ghost_w,
                DRAG_GHOST_H,
                &self.card_drag_label,
                UIStyle {
                    bg_color: DRAG_GHOST_BG,
                    text_color: DRAG_GHOST_TEXT,
                    font_size: DRAG_GHOST_FONT_SIZE,
                    text_align: TextAlign::Center,
                    corner_radius: color::CARD_RADIUS,
                    ..UIStyle::default()
                },
            ));
            self.card_drag_indicator_id = Some(tree.add_panel(
                None,
                col_x + DRAG_INDICATOR_INSET,
                -100.0,
                col_w - DRAG_INDICATOR_INSET * 2.0,
                DRAG_INDICATOR_H,
                UIStyle {
                    bg_color: DRAG_INDICATOR_COLOR,
                    corner_radius: color::HAIRLINE_RADIUS,
                    ..UIStyle::default()
                },
            ));
            tree.end_region(region, region_start);
            self.card_drag_region_root = Some(region.root);

            return true;
        }
        false
    }

    /// Update card drag ghost + indicator during drag.
    pub fn update_card_drag(&mut self, pos: Vec2, tree: &mut UITree) {
        if !self.card_drag_active {
            return;
        }

        let vp = self.viewport_rect;
        // Single full-width active column — both tabs drag within it.
        let col_x = vp.x + COLUMN_PAD;
        let col_w = (vp.width - COLUMN_PAD * 2.0).max(0.0);
        let ghost_w = (col_w - 24.0).min(160.0);

        // Position ghost centered on cursor, clamped to column
        let ghost_x = (pos.x - ghost_w * 0.5).clamp(
            col_x + DRAG_INDICATOR_INSET,
            col_x + col_w - ghost_w - DRAG_INDICATOR_INSET,
        );
        let ghost_y = (pos.y - DRAG_GHOST_H * 0.5).clamp(vp.y, vp.y + vp.height - DRAG_GHOST_H);

        if let Some(ghost_id) = self.card_drag_ghost_id {
            tree.set_bounds(
                ghost_id,
                Rect::new(ghost_x, ghost_y, ghost_w, DRAG_GHOST_H),
            );
        }

        // Compute target card index from Y position. Hit-test against live
        // tree bounds (scroll-current, animation-current), not the
        // build-time `card_y` snapshot / animated `compute_height()` — those
        // go stale by exactly the scroll delta on the in-place scroll path
        // (BUG-265). Cards without a live rect (never built) are skipped.
        let tab = self.card_drag_tab;
        let (target, indicator_y) = {
            let cards = self.cards_for_tab(tab);
            let card_count = cards.len();
            let mut t = card_count; // default: after last card
            for (i, card) in cards.iter().enumerate() {
                let Some(b) = card.live_bounds(tree) else {
                    continue;
                };
                let mid = b.y + b.height * 0.5;
                if pos.y < mid {
                    t = i;
                    break;
                }
            }
            let iy = if t < card_count {
                cards[t].live_bounds(tree).map(|b| b.y).unwrap_or(vp.y)
            } else if card_count > 0 {
                cards[card_count - 1]
                    .live_bounds(tree)
                    .map(|b| b.y + b.height)
                    .unwrap_or(vp.y)
            } else {
                vp.y
            };
            (t, iy)
        };
        self.card_drag_target_index = target;

        if let Some(indicator_id) = self.card_drag_indicator_id {
            tree.set_bounds(
                indicator_id,
                Rect::new(
                    col_x + DRAG_INDICATOR_INSET,
                    indicator_y - DRAG_INDICATOR_H * 0.5,
                    col_w - DRAG_INDICATOR_INSET * 2.0,
                    DRAG_INDICATOR_H,
                ),
            );
        }
    }

    /// End card drag — restore dimming, hide ghost/indicator, return reorder action.
    /// Supports multi-select: if dragged card is part of a selection, moves all selected.
    pub fn end_card_drag(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if !self.card_drag_active {
            return Vec::new();
        }

        let src = self.card_drag_source_index;
        let tab = self.card_drag_tab;
        let from = self.card_drag_effect_index;
        let to_card = self.card_drag_target_index;

        // Check if dragged card is part of a multi-selection
        let dragged_id = self
            .cards_for_tab(tab)
            .get(src)
            .map(|c| c.effect_id().clone());
        let sel = self.selection_set_mut(tab);
        let is_multi = dragged_id
            .as_ref()
            .is_some_and(|id| sel.len() > 1 && sel.contains(id));

        // Restore source card border + compute target effect index
        let to_fx = {
            // Restore dimming on all selected cards (or just source)
            if is_multi {
                let sel_ids: HashSet<EffectId> = self.selection_set_mut(tab).clone();
                let cards = self.cards_for_tab(tab);
                for card in cards {
                    if sel_ids.contains(card.effect_id()) {
                        card.set_drag_dimmed(tree, false);
                    }
                }
            } else if let Some(card) = self.cards_for_tab(tab).get(src) {
                card.set_drag_dimmed(tree, false);
            }
            let cards = self.cards_for_tab(tab);
            if to_card < cards.len() {
                cards[to_card].effect_index()
            } else if !cards.is_empty() {
                // After-last drop: one past the HIGHEST effect index in the
                // tab's cards, not `cards.last()`'s index — the tab's cards
                // are a contiguous run of the flat effects list today, but
                // list order isn't guaranteed to track index order, and
                // `.last()` silently breaks the moment it doesn't (BUG-265
                // root cause 3).
                cards.iter().map(|c| c.effect_index()).max().unwrap() + 1
            } else {
                0
            }
        };

        // Hide ghost + indicator (move offscreen)
        if let Some(ghost_id) = self.card_drag_ghost_id {
            tree.set_bounds(ghost_id, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
        if let Some(indicator_id) = self.card_drag_indicator_id {
            tree.set_bounds(indicator_id, Rect::new(0.0, -100.0, 0.0, 0.0));
        }

        self.card_drag_active = false;
        self.card_drag_ghost_id = None;
        self.card_drag_indicator_id = None;
        self.card_drag_region_root = None;

        if is_multi {
            // Multi-select: move all selected effects as a group
            let sel_ids = self.selection_set_mut(tab).clone();
            let cards = self.cards_for_tab(tab);
            // Convert selected IDs to sorted effect indices
            let mut effect_indices: Vec<usize> = cards
                .iter()
                .filter(|c| sel_ids.contains(c.effect_id()))
                .map(|c| c.effect_index())
                .collect();
            effect_indices.sort_unstable();
            if !effect_indices.is_empty() {
                vec![PanelAction::EffectReorderGroup(effect_indices, to_fx)]
            } else {
                Vec::new()
            }
        } else if to_fx != from && to_fx != from + 1 {
            vec![PanelAction::EffectReorder(from, to_fx)]
        } else {
            Vec::new()
        }
    }

    /// Find which card's drag handle matches the given node_id.
    /// Returns (tab, card_index_in_vec, effect_index, effect_name).
    fn find_drag_handle(&self, node_id: NodeId) -> Option<(InspectorTab, usize, usize, String)> {
        // No scope gate: `is_drag_handle` is false on a non-live card, so only the
        // active scope's cards can match (the node range is the source of truth).
        for (i, card) in self.effects[Self::SCOPE_MASTER].iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((
                    InspectorTab::Master,
                    i,
                    card.effect_index(),
                    card.effect_name().to_string(),
                ));
            }
        }
        for (i, card) in self.effects[Self::SCOPE_LAYER].iter().enumerate() {
            if card.is_drag_handle(node_id) {
                return Some((
                    InspectorTab::Layer,
                    i,
                    card.effect_index(),
                    card.effect_name().to_string(),
                ));
            }
        }
        None
    }

    fn cards_for_tab(&self, tab: InspectorTab) -> &[ParamCardPanel] {
        &self.effects[Self::scope_idx(tab)]
    }

    fn cards_for_tab_mut(&mut self, tab: InspectorTab) -> &mut Vec<ParamCardPanel> {
        &mut self.effects[Self::scope_idx(tab)]
    }

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

    fn route_click(&mut self, node_id: NodeId, modifiers: Modifiers, tree: &UITree) -> Vec<PanelAction> {
        // Tab strip — selecting a tab mirrors the timeline selection.
        if let Some((_, tab)) = self.tab_node_ids.iter().find(|(id, _)| *id == node_id) {
            return vec![PanelAction::SelectInspectorTab(*tab)];
        }
        // Collapse-all / expand-all — resolve the target state from the active
        // column's current cards (collapse if any open, else expand).
        if self.collapse_all_btn_id == Some(node_id) {
            let collapsed = self.any_active_card_expanded();
            return vec![PanelAction::SetAllCardsCollapsed { collapsed }];
        }
        // §6b — compact toggle: flip global mod-drawer visibility (UI-only). Flip
        // here and return a structural no-op so the inspector rebuilds with the
        // new state propagated to every card.
        if self.compact_toggle_btn_id == Some(node_id) {
            self.mods_compact = !self.mods_compact;
            return vec![PanelAction::ModsCompactToggled];
        }
        // Add Effect buttons
        if self.add_master_effect_btn == Some(node_id) {
            return vec![PanelAction::AddEffectClicked(InspectorTab::Master)];
        }
        if self.add_layer_effect_btn == Some(node_id) {
            return vec![PanelAction::AddEffectClicked(InspectorTab::Layer)];
        }
        if let Some(target) = self.find_target_for_node(node_id) {
            self.update_last_effect_tab(&target);
            match target {
                PressedTarget::Macros => self.macros_panel.handle_click(node_id),
                PressedTarget::AudioTriggers => self.audio_trigger_section.handle_click(node_id),
                PressedTarget::MasterChrome => self.master_chrome.handle_click(node_id),
                PressedTarget::LayerChrome => self.layer_chrome.handle_click(node_id),
                PressedTarget::ClipChrome => self.clip_chrome.handle_click(node_id),
                PressedTarget::MasterEffect(i) => {
                    let mut actions = self.effects[Self::SCOPE_MASTER]
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id, tree))
                        .unwrap_or_default();

                    if actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::EffectCardClicked(_)))
                    {
                        self.on_effect_card_clicked(InspectorTab::Master, i, modifiers);
                    } else if !self.is_effect_target_selected(&PressedTarget::MasterEffect(i)) {
                        // Only auto-select if not already in multi-selection
                        self.auto_select_effect(&PressedTarget::MasterEffect(i));
                    }
                    let ei = self.effects[Self::SCOPE_MASTER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0);
                    if !actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::EffectCardClicked(_)))
                    {
                        actions.insert(0, PanelAction::EffectCardClicked(ei));
                    }
                    actions
                }
                PressedTarget::LayerEffect(i) => {
                    let mut actions = self.effects[Self::SCOPE_LAYER]
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id, tree))
                        .unwrap_or_default();

                    if actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::EffectCardClicked(_)))
                    {
                        self.on_effect_card_clicked(InspectorTab::Layer, i, modifiers);
                    } else if !self.is_effect_target_selected(&PressedTarget::LayerEffect(i)) {
                        self.auto_select_effect(&PressedTarget::LayerEffect(i));
                    }
                    let ei = self.effects[Self::SCOPE_LAYER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0);
                    if !actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::EffectCardClicked(_)))
                    {
                        actions.insert(0, PanelAction::EffectCardClicked(ei));
                    }
                    actions
                }
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_click(node_id, tree))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    fn route_pointer_down(
        &mut self,
        node_id: NodeId,
        pos: Vec2,
        modifiers: Modifiers,
        tree: &UITree,
    ) -> Vec<PanelAction> {
        let target = self.find_target_for_node(node_id);
        self.pressed_target = target;
        // Record which tab this interaction targets (survives drag_end)
        if let Some(ref t) = target {
            self.update_last_effect_tab(t);
            // Auto-select on pointer-down ONLY when:
            // 1. No selection modifiers are held (shift/ctrl defer to Click handler)
            // 2. The target is not already selected (preserve multi-selection for
            //    functional buttons like chevron/toggle on selected effects)
            if !modifiers.shift
                && !modifiers.ctrl
                && !modifiers.command
                && !self.is_effect_target_selected(t)
            {
                self.auto_select_effect(t);
            }
        }

        if let Some(target) = target {
            // For effect targets, prepend EffectCardClicked to trigger visual update
            let select_action = match target {
                PressedTarget::MasterEffect(i) => Some(PanelAction::EffectCardClicked(
                    self.effects[Self::SCOPE_MASTER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0),
                )),
                PressedTarget::LayerEffect(i) => Some(PanelAction::EffectCardClicked(
                    self.effects[Self::SCOPE_LAYER]
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0),
                )),
                _ => None,
            };

            let mut actions = match target {
                PressedTarget::Macros => self.macros_panel.handle_press(node_id, pos.x),
                PressedTarget::AudioTriggers => {
                    self.audio_trigger_section.handle_press(node_id, pos.x)
                }
                PressedTarget::MasterChrome => {
                    self.master_chrome.handle_pointer_down(node_id, pos)
                }
                PressedTarget::LayerChrome => {
                    self.layer_chrome.handle_pointer_down(node_id, pos)
                }
                PressedTarget::ClipChrome => self.clip_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::MasterEffect(i) => self.effects[Self::SCOPE_MASTER]
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self.effects[Self::SCOPE_LAYER]
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_pointer_down(node_id, pos, tree))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => {
                    self.dragging_scrollbar = true;
                    self.dragging_scrollbar_master = Some(node_id) == self.master_scroll.track_id()
                        || Some(node_id) == self.master_scroll.thumb_id();
                    Vec::new()
                }
            };

            // Prepend EffectCardClicked so dispatch applies selection visuals
            if let Some(sa) = select_action {
                actions.insert(0, sa);
            }
            actions
        } else {
            Vec::new()
        }
    }

    /// Build the whole inspector column into an explicit `rect`, decoupled from
    /// `ScreenLayout::inspector()` so the graph-editor window can host the same
    /// column in its right lane (`dock.right`). `Panel::build` is the thin
    /// wrapper that passes `layout.inspector()`.
    pub fn build_in_rect(&mut self, tree: &mut UITree, rect: Rect) {
        self.cache_first_node = tree.count();

        // Add-effect button ids are reassigned by node index every rebuild, but
        // each is only *set* inside its `!collapsed` build branch. Clear them up
        // front so a collapsed/hidden section can't leave a stale id pointing at
        // an index another node now occupies — e.g. the generator card's Change
        // button inheriting a stale add-layer-effect id and opening the effect
        // browser instead of the generator picker. (The exact-id checks in
        // handle_click run before the range-based find_target_for_node.)
        self.add_master_effect_btn = None;
        self.add_layer_effect_btn = None;

        // Range truthfulness (the single invariant the rest of this panel leans
        // on): a sub-panel's (first_node, node_count) must describe what it built
        // THIS frame. Only the active scope's sections build below, so reset every
        // section's range up front — a section left un-built then honestly reports
        // an empty range. `node_count() > 0` ("live this frame") becomes the one
        // signal every consumer keys off (hit routing, intents, type-in, the
        // sub-region cache, selection visuals), so an inactive scope can never
        // alias the active scope's node indices. This is what makes the tab system
        // safe by construction instead of by a `*_visible()` guard repeated at
        // every read site. Runs before the zero-width early-return so a collapsed
        // inspector also leaves no stale ranges.
        self.master_chrome.clear_nodes();
        self.layer_chrome.clear_nodes();
        self.clip_chrome.clear_nodes();
        self.audio_trigger_section.clear_nodes();
        if let Some(gp) = self.gen_params.as_mut() {
            gp.clear_nodes();
        }
        for card in self.effects.iter_mut().flatten() {
            card.clear_nodes();
        }

        if rect.width <= 0.0 {
            return;
        }

        self.viewport_rect = rect;

        // §6b — propagate global compact mode to every card before layout, so
        // compute_height and build agree on whether drawers are hidden.
        self.apply_mods_compact();

        // Background panel
        self.bg_panel_id = Some(tree.add_panel(
            None,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                ..UIStyle::default()
            },
        ));

        // Macros strip pinned to the very top of the inspector. The macro bank is
        // a global (project-level) control, so it sits ABOVE the per-scope tab
        // strip rather than reading as part of any one inspector (Clip/Layer/
        // Master). Built AFTER the columns for z-order — see the build call near
        // the end of this fn. height() is a pure getter, safe to read here.
        let macros_h = self.macros_panel.height();
        let macros_y = rect.y;

        // One content box shared by the macros strip, the tab strip, AND the
        // section cards, so all three line up on the same left and right inset
        // (the tabs used to span the full width and visibly stick out past the
        // cards). `content_w` reserves the scrollbar gutter on the right; the
        // cards' right edge lands at `col_x + content_w`, so tabs/macros sized to
        // it match exactly.
        let col_x = rect.x + COLUMN_PAD;
        let content_w = (rect.width - COLUMN_PAD * 2.0 - SCROLLBAR_W).max(0.0);
        let full_col_w = (rect.width - COLUMN_PAD * 2.0).max(0.0);

        // Tab strip below the macros: the rungs of the current selection
        // (Clip · Layer · Group · Master), active one highlighted. Inset to the
        // shared content box.
        let tab_h = TAB_STRIP_HEIGHT;
        let tab_y = macros_y + macros_h + 2.0; // 2px gap below macros
        self.build_tab_strip(tree, Rect::new(col_x, tab_y, content_w, tab_h));
        let (master_col_w, layer_col_w) = if self.master_visible() {
            (full_col_w, 0.0)
        } else {
            (0.0, full_col_w)
        };
        // Aliases so the per-section build blocks below read unchanged.
        let left_x = col_x;
        let right_x = col_x;
        let left_content_w = if self.master_visible() { content_w } else { 0.0 };
        let right_content_w = if self.master_visible() { 0.0 } else { content_w };
        self.column_split_x = if self.master_visible() {
            rect.x + rect.width
        } else {
            rect.x
        };

        // Columns start below the tab strip.
        let columns_y = tab_y + tab_h + 2.0; // 2px gap below tabs
        let columns_h = (rect.y + rect.height - columns_y).max(0.0);
        self.columns_y = columns_y;
        self.columns_height = columns_h;

        // `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1 stopgap removal: this used to
        // be a bespoke, root-parented `ClipRegion` (`content_clip_id`) — the
        // BUG-060 hand-clip the design forbids by name — covering
        // `(rect.x, columns_y, rect.width, columns_h)` so no card content
        // could paint past the inspector's own bottom edge. It's gone: the
        // outer `begin_region` the inspector now builds under (`ui_root.rs`)
        // clips the WHOLE inspector rect unconditionally, so that job is
        // structural now, not this panel's to do by hand.
        //
        // It was NOT, however, doing nothing — decided empirically, not by
        // reading (`inspector.rs`'s
        // `content_clip_prevents_scrolled_columns_painting_over_the_tab_strip`
        // test forced a scroll far enough to push several cards' raw bounds
        // above `columns_y`, into the pinned tab-strip's territory, with
        // this clip's `CLIPS_CHILDREN` flag removed as a controlled
        // experiment — and the GPU-cull replica still reported zero pixels
        // reaching the tab strip). The reason: `master_scroll`/`layer_scroll`
        // (`ScrollContainer::begin`, just below) each mint their OWN
        // `CLIPS_CHILDREN` clip at the SAME `columns_y` top edge — this
        // node's Y-range was always a strict subset of whichever column is
        // active. Deleted, not kept-and-reparented: proven redundant, not
        // merely unproven-necessary.

        // ── MASTER COLUMN (full width when active, else collapsed) ──
        let left_clip_rect = Rect::new(left_x, columns_y, master_col_w, columns_h);
        self.master_scroll.begin(tree, left_clip_rect);
        let left_start = tree.count();

        {
            let mut cy = self.master_scroll.content_y(0.0);
            if self.master_visible() {
                let section_h = self.master_column_height();
                chrome::materialize(
                    tree,
                    &section_card_view(),
                    Rect::new(left_x, cy, left_content_w, section_h),
                );
                cy += SECTION_CARD_PAD;

                let inner_x = left_x + SECTION_INSET;
                let inner_w = left_content_w - SECTION_INSET * 2.0;

                let chrome_h = self.master_chrome.compute_height();
                self.master_chrome
                    .build(tree, Rect::new(inner_x, cy, inner_w, chrome_h));
                cy += chrome_h;

                if !self.master_chrome.is_collapsed() {
                    for card in &mut self.effects[Self::SCOPE_MASTER] {
                        let card_h = card.compute_height();
                        card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                        cy += card_h + SECTION_GAP;
                    }
                    self.add_master_effect_btn = chrome::materialize(
                        tree,
                        &add_effect_button_view(),
                        Rect::new(inner_x, cy, inner_w, ADD_EFFECT_BTN_H),
                    )
                    .into_iter()
                    .find(|(k, _)| *k == KEY_ADD_EFFECT_BTN)
                    .map(|(_, id)| id);
                    cy += ADD_EFFECT_BTN_H + SECTION_GAP;
                }
            }
            // D17 "delete collapse" — dying cards render below everything
            // else in this column (append-only, see `master_dying`'s doc
            // comment), still shrinking toward zero height until their
            // exit-state finishes. Recomputes the same inset `inner_x`/
            // `inner_w` the live-card loop above used (out of scope here —
            // that `let` lives inside the `master_visible()` block).
            if !self.master_dying.is_empty() {
                let inner_x = left_x + SECTION_INSET;
                let inner_w = left_content_w - SECTION_INSET * 2.0;
                for card in &mut self.master_dying {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
            }
        }
        self.master_scroll.reparent_content(tree, left_start);
        self.master_scroll
            .build_scrollbar(tree, left_x + left_content_w, &SCROLLBAR_STYLE);

        // ── LAYER/GROUP/CLIP COLUMN (full width when active, else collapsed) ──
        let right_clip_rect = Rect::new(right_x, columns_y, layer_col_w, columns_h);
        self.layer_scroll.begin(tree, right_clip_rect);
        let right_start = tree.count();

        {
            let mut cy = self.layer_scroll.content_y(0.0);

            // Layer section — includes gen params above layer effects
            if self.layer_visible() {
                let section_h = self.layer_column_height();
                chrome::materialize(
                    tree,
                    &section_card_view(),
                    Rect::new(right_x, cy, right_content_w, section_h),
                );
                cy += SECTION_CARD_PAD;

                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;

                let chrome_h = self.layer_chrome.compute_height();
                self.layer_chrome
                    .build(tree, Rect::new(inner_x, cy, inner_w, chrome_h));
                cy += chrome_h;

                if !self.layer_chrome.is_collapsed() {
                    // AUDIO TRIGGERS (P3b) — pinned at the top of the layer's
                    // detail content, above gen params and layer effects.
                    let at_h = self.audio_trigger_section.height();
                    self.audio_trigger_section
                        .build(tree, Rect::new(inner_x, cy, inner_w, at_h));
                    cy += at_h + SECTION_GAP;

                    if let Some(ref mut gp) = self.gen_params {
                        let gp_h = gp.compute_height();
                        gp.build(tree, Rect::new(inner_x, cy, inner_w, gp_h));
                        cy += gp_h + SECTION_GAP;
                    }

                    for card in &mut self.effects[Self::SCOPE_LAYER] {
                        let card_h = card.compute_height();
                        card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                        cy += card_h + SECTION_GAP;
                    }
                    self.add_layer_effect_btn = chrome::materialize(
                        tree,
                        &add_effect_button_view(),
                        Rect::new(inner_x, cy, inner_w, ADD_EFFECT_BTN_H),
                    )
                    .into_iter()
                    .find(|(k, _)| *k == KEY_ADD_EFFECT_BTN)
                    .map(|(_, id)| id);
                    cy += ADD_EFFECT_BTN_H + SECTION_GAP;
                }
            }
            // D17 "delete collapse" — see the matching comment in the master
            // column above. `inner_x`/`inner_w` recomputed the same way (out
            // of scope here, inside `layer_visible()`'s block).
            if !self.layer_dying.is_empty() {
                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;
                for card in &mut self.layer_dying {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
            }

            // Clip section — its own card below the layer section, shown when a
            // clip is selected. Holds the per-clip chrome (BPM / warp / loop).
            if self.clip_visible() && self.clip_chrome.has_clip() {
                let clip_top = self.layer_scroll.content_y(0.0) + self.layer_column_height();
                let section_h =
                    SECTION_CARD_PAD + self.clip_chrome.compute_height() + SECTION_CARD_PAD;
                chrome::materialize(
                    tree,
                    &section_card_view(),
                    Rect::new(right_x, clip_top, right_content_w, section_h),
                );
                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;
                let chrome_h = self.clip_chrome.compute_height();
                self.clip_chrome.build(
                    tree,
                    Rect::new(inner_x, clip_top + SECTION_CARD_PAD, inner_w, chrome_h),
                );
            }
            // No `else` to clear the clip range: the up-front reset already left it
            // empty, so an un-built clip section reports not-live without a second
            // bookkeeping site.
        }
        self.layer_scroll.reparent_content(tree, right_start);
        self.layer_scroll
            .build_scrollbar(tree, right_x + right_content_w, &SCROLLBAR_STYLE);

        // Both columns' scroll clips (`ScrollContainer::begin` always mints
        // its clip node with `parent: None`) are still tree roots here — no
        // longer swept under a bespoke `content_clip_id` (removed, see the
        // comment above `columns_y`'s scroll containers begin). The caller's
        // `begin_region`/`end_region` wrap (`ui_root.rs`) sweeps them
        // directly, same as every other `None`-rooted node this panel
        // builds (D1/D4).

        // ── MACROS STRIP (pinned to the top, above the tab strip; built last so
        // it draws on top of any column content) ──
        let macros_rect = Rect::new(left_x, macros_y, content_w, macros_h);
        self.macros_panel.build(tree, macros_rect);

        self.update_scroll_bounds();
        self.update_scrollbar(tree);

        self.cache_node_count = tree.count() - self.cache_first_node;
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
        author_panel.configure_master_effects(&[config.clone()]);
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
            PanelAction::EffectReorder(from, to) => {
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
            PanelAction::EffectReorder(from, to) => {
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
            PanelAction::EffectReorder(_from, to) => {
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
            assert!(matches!(actions[0], PanelAction::MasterCollapseToggle));
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
