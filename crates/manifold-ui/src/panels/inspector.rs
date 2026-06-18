use super::clip_chrome::ClipChromePanel;
use super::layer_chrome::LayerChromePanel;
use super::macros_panel::MacrosPanel;
use super::master_chrome::MasterChromePanel;
use super::param_card::{ParamCardConfig, ParamCardPanel};
use super::{InspectorTab, Panel, PanelAction};
use crate::color;
use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use manifold_core::EffectId;
use manifold_core::LayerId;
use std::collections::HashSet;

// ── Layout constants ────────────────────────────────────────────
const SECTION_GAP: f32 = 6.0;
const SECTION_CARD_RADIUS: f32 = 4.0;
const SECTION_CARD_PAD: f32 = 6.0;
const SECTION_CARD_BG: Color32 = Color32::new(22, 22, 23, 255);
const SECTION_CARD_BORDER: Color32 = Color32::new(50, 50, 54, 255);
const COLUMN_PAD: f32 = 4.0;
const SECTION_INSET: f32 = 4.0; // horizontal padding inside section cards

const SCROLLBAR_STYLE: ScrollbarStyle = ScrollbarStyle {
    track_color: color::SCROLLBAR_TRACK_C32,
    thumb_color: color::SCROLLBAR_THUMB_C32,
    thumb_hover_color: color::SCROLLBAR_THUMB_HOVER_C32,
    corner_radius: 2.0,
};

const ADD_EFFECT_BTN_H: f32 = 26.0;

// ── Effect card drag-reorder constants (Unity EffectsListBitmapPanel) ──
const DRAG_GHOST_H: f32 = 24.0;
const DRAG_GHOST_FONT_SIZE: u16 = color::FONT_BODY;
const DRAG_GHOST_BG: Color32 = Color32::new(60, 80, 120, 200);
const DRAG_GHOST_TEXT: Color32 = Color32::new(220, 220, 230, 255);
const DRAG_INDICATOR_H: f32 = 2.0;
const DRAG_INDICATOR_INSET: f32 = 4.0;
const DRAG_INDICATOR_COLOR: Color32 = color::ACCENT_BLUE_C32;
const ADD_EFFECT_BTN_BG: Color32 = color::ADD_EFFECT_BTN_BG_C32;
const ADD_EFFECT_BTN_HOVER: Color32 = color::ADD_EFFECT_BTN_HOVER_C32;
const ADD_EFFECT_BTN_TEXT: Color32 = color::ADD_EFFECT_BTN_TEXT_C32;

// ── Pressed target (for drag routing) ───────────────────────────

#[derive(Debug, Clone, Copy)]
enum PressedTarget {
    Macros,
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
    master_chrome: MasterChromePanel,
    layer_chrome: LayerChromePanel,
    clip_chrome: ClipChromePanel,
    master_effects: Vec<ParamCardPanel>,
    layer_effects: Vec<ParamCardPanel>,
    gen_params: Option<ParamCardPanel>,

    // Section visibility
    master_visible: bool,
    layer_visible: bool,
    clip_visible: bool,

    // Add Effect button node IDs
    add_master_effect_btn: i32,
    add_layer_effect_btn: i32,

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

    // Event routing
    pressed_target: Option<PressedTarget>,
    /// Remembers which inspector tab (Master/Layer/Clip) the last effect
    /// interaction targeted. Survives across drag_end so dispatch can
    /// route effect actions to the correct data location.
    last_effect_tab: InspectorTab,

    // Background
    bg_panel_id: i32,

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
    card_drag_ghost_id: i32,
    card_drag_indicator_id: i32,
    card_drag_label: String,

    // Cache tracking
    cache_first_node: usize,
    cache_node_count: usize,
}

impl InspectorCompositePanel {
    pub fn new() -> Self {
        Self {
            macros_panel: MacrosPanel::new(),
            master_chrome: MasterChromePanel::new(),
            layer_chrome: LayerChromePanel::new(),
            clip_chrome: ClipChromePanel::new(),
            master_effects: Vec::new(),
            layer_effects: Vec::new(),
            gen_params: None,
            master_visible: true,
            layer_visible: true,
            clip_visible: true,
            add_master_effect_btn: -1,
            add_layer_effect_btn: -1,
            master_scroll: ScrollContainer::new(),
            layer_scroll: ScrollContainer::new(),
            viewport_rect: Rect::ZERO,
            column_split_x: 0.0,
            columns_y: 0.0,
            columns_height: 0.0,
            dragging_scrollbar: false,
            dragging_scrollbar_master: false,
            pressed_target: None,
            last_effect_tab: InspectorTab::Layer,
            bg_panel_id: -1,
            selected_master_ids: HashSet::new(),
            selected_layer_ids: HashSet::new(),
            last_clicked_master: None,
            last_clicked_layer: None,
            card_drag_active: false,
            card_drag_tab: InspectorTab::Master,
            card_drag_source_index: 0,
            card_drag_effect_index: 0,
            card_drag_target_index: 0,
            card_drag_ghost_id: -1,
            card_drag_indicator_id: -1,
            card_drag_label: String::new(),
            cache_first_node: usize::MAX,
            cache_node_count: 0,
        }
    }

    // ── Configuration ─────────────────────────────────────────────

    pub fn set_section_visible(&mut self, section: InspectorTab, visible: bool) {
        match section {
            InspectorTab::Master => self.master_visible = visible,
            InspectorTab::Layer => self.layer_visible = visible,
            InspectorTab::Clip => self.clip_visible = visible,
        }
    }

    /// Sub-region node ranges for incremental cache re-rendering.
    /// Returns (node_start, node_end) for each sub-panel: chrome panels,
    /// effect cards, gen params. Used by the cache manager to detect which
    /// parts of the inspector changed and only re-render those.
    pub fn sub_region_ranges(&self) -> Vec<(usize, usize)> {
        let mut ranges =
            Vec::with_capacity(4 + self.master_effects.len() + self.layer_effects.len() + 1);
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
        for card in &self.master_effects {
            push(&mut ranges, card.first_node(), card.node_count());
        }
        push(
            &mut ranges,
            self.layer_chrome.first_node(),
            self.layer_chrome.node_count(),
        );
        push(
            &mut ranges,
            self.clip_chrome.first_node(),
            self.clip_chrome.node_count(),
        );
        if let Some(ref gp) = self.gen_params {
            push(&mut ranges, gp.first_node(), gp.node_count());
        }
        for card in &self.layer_effects {
            push(&mut ranges, card.first_node(), card.node_count());
        }
        ranges
    }

    pub fn configure_master_effects(&mut self, configs: &[ParamCardConfig]) {
        self.master_effects = Self::build_cards(configs);
    }

    pub fn configure_layer_effects(&mut self, configs: &[ParamCardConfig]) {
        self.layer_effects = Self::build_cards(configs);
    }

    pub fn configure_gen_params(
        &mut self,
        config: Option<&ParamCardConfig>,
        layer_id: Option<LayerId>,
    ) {
        // The generator card is a single optional, distinct from the effect
        // lists (it carries no EffectId and is outside the selection +
        // drag-reorder model), so it isn't built through `build_cards`.
        // `set_layer_id` is applied before `configure` per its contract.
        self.gen_params = config.map(|cfg| {
            let mut panel = ParamCardPanel::new();
            panel.set_layer_id(layer_id);
            panel.configure(cfg);
            panel
        });
    }

    /// Build a fresh effect-card panel per config, in order. Shared by the
    /// master + layer effect lists (the only structural difference between
    /// them is which `Vec` the result lands in).
    fn build_cards(configs: &[ParamCardConfig]) -> Vec<ParamCardPanel> {
        configs
            .iter()
            .map(|cfg| {
                let mut card = ParamCardPanel::new();
                card.configure(cfg);
                card
            })
            .collect()
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
        let cards = match tab {
            InspectorTab::Master => &self.master_effects,
            InspectorTab::Layer | InspectorTab::Clip => &self.layer_effects,
        };
        cards
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

    pub fn master_effect_mut(&mut self, idx: usize) -> Option<&mut ParamCardPanel> {
        self.master_effects.get_mut(idx)
    }
    pub fn layer_effect_mut(&mut self, idx: usize) -> Option<&mut ParamCardPanel> {
        self.layer_effects.get_mut(idx)
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

    pub fn is_dragging(&self) -> bool {
        self.dragging_scrollbar
            || self.card_drag_active
            || self.macros_panel.is_dragging()
            || self.master_chrome.is_dragging()
            || self.layer_chrome.is_dragging()
            || self.clip_chrome.is_dragging()
            || self.master_effects.iter().any(|e| e.is_dragging())
            || self.layer_effects.iter().any(|e| e.is_dragging())
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

    /// Content height for the master column (left).
    fn master_column_height(&self) -> f32 {
        if !self.master_visible {
            return 0.0;
        }
        let mut h = SECTION_CARD_PAD + self.master_chrome.compute_height();
        if !self.master_chrome.is_collapsed() {
            for card in &self.master_effects {
                h += card.compute_height() + SECTION_GAP;
            }
            h += ADD_EFFECT_BTN_H + SECTION_GAP;
        }
        h + SECTION_CARD_PAD
    }

    /// Content height for the layer column (right).
    /// Order: layer chrome → gen params → layer effects → add effect button.
    fn layer_column_height(&self) -> f32 {
        let mut h = 0.0;
        if self.layer_visible {
            h += SECTION_CARD_PAD + self.layer_chrome.compute_height();
            if !self.layer_chrome.is_collapsed() {
                // Gen params sit above layer effects
                if let Some(ref gp) = self.gen_params {
                    h += gp.compute_height() + SECTION_GAP;
                }
                for card in &self.layer_effects {
                    h += card.compute_height() + SECTION_GAP;
                }
                h += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            h += SECTION_CARD_PAD + SECTION_GAP;
        }
        h
    }

    fn update_scroll_bounds(&mut self) {
        self.master_scroll
            .set_content_height(self.master_column_height());
        self.layer_scroll
            .set_content_height(self.layer_column_height());
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
            PressedTarget::LayerChrome | PressedTarget::LayerEffect(_) => {
                self.last_effect_tab = InspectorTab::Layer;
            }
            PressedTarget::ClipChrome | PressedTarget::GenParam => {
                self.last_effect_tab = InspectorTab::Clip;
            }
            PressedTarget::Macros | PressedTarget::Scrollbar => {}
        }
    }

    // ── Effect selection (Unity EffectSelectionManager) ─────────

    /// Get the selection set and cards vec for a given tab.
    fn selection_for_tab(&self, tab: InspectorTab) -> (&HashSet<EffectId>, &[ParamCardPanel]) {
        match tab {
            InspectorTab::Master => (&self.selected_master_ids, &self.master_effects),
            InspectorTab::Layer | InspectorTab::Clip => {
                (&self.selected_layer_ids, &self.layer_effects)
            }
        }
    }

    fn last_clicked_for_tab(&self, tab: InspectorTab) -> Option<&EffectId> {
        match tab {
            InspectorTab::Master => self.last_clicked_master.as_ref(),
            InspectorTab::Layer | InspectorTab::Clip => self.last_clicked_layer.as_ref(),
        }
    }

    fn set_last_clicked_for_tab(&mut self, tab: InspectorTab, id: Option<EffectId>) {
        match tab {
            InspectorTab::Master => self.last_clicked_master = id,
            InspectorTab::Layer | InspectorTab::Clip => self.last_clicked_layer = id,
        }
    }

    fn selection_set_mut(&mut self, tab: InspectorTab) -> &mut HashSet<EffectId> {
        match tab {
            InspectorTab::Master => &mut self.selected_master_ids,
            InspectorTab::Layer | InspectorTab::Clip => &mut self.selected_layer_ids,
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
        for card in self
            .master_effects
            .iter_mut()
            .chain(self.layer_effects.iter_mut())
        {
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

    fn find_target_for_node(&self, node_id: u32) -> Option<PressedTarget> {
        let idx = node_id as usize;
        let id = node_id as i32;

        // Macros panel (above both columns)
        if self.macros_panel.owns_node(node_id) {
            return Some(PressedTarget::Macros);
        }

        // Scrollbars
        if id == self.master_scroll.track_id()
            || id == self.master_scroll.thumb_id()
            || id == self.layer_scroll.track_id()
            || id == self.layer_scroll.thumb_id()
        {
            return Some(PressedTarget::Scrollbar);
        }

        // Master section
        if self.master_visible {
            if in_range(
                idx,
                self.master_chrome.first_node(),
                self.master_chrome.node_count(),
            ) {
                return Some(PressedTarget::MasterChrome);
            }
            for (i, card) in self.master_effects.iter().enumerate() {
                if in_range(idx, card.first_node(), card.node_count()) {
                    return Some(PressedTarget::MasterEffect(i));
                }
            }
        }

        // Layer section
        if self.layer_visible {
            if in_range(
                idx,
                self.layer_chrome.first_node(),
                self.layer_chrome.node_count(),
            ) {
                return Some(PressedTarget::LayerChrome);
            }
            for (i, card) in self.layer_effects.iter().enumerate() {
                if in_range(idx, card.first_node(), card.node_count()) {
                    return Some(PressedTarget::LayerEffect(i));
                }
            }
        }

        // Clip section
        if self.clip_visible {
            if in_range(
                idx,
                self.clip_chrome.first_node(),
                self.clip_chrome.node_count(),
            ) {
                return Some(PressedTarget::ClipChrome);
            }
            if let Some(ref gp) = self.gen_params
                && in_range(idx, gp.first_node(), gp.node_count())
            {
                return Some(PressedTarget::GenParam);
            }
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
    /// Returns None if no drag is active.
    pub fn card_drag_first_node(&self) -> Option<usize> {
        if self.card_drag_active && self.card_drag_ghost_id >= 0 {
            Some(self.card_drag_ghost_id as usize)
        } else {
            None
        }
    }

    /// Route drag events to the pressed sub-panel.
    /// Called from UIRoot::process_events (not through Panel::handle_event)
    /// because it needs &mut UITree for slider visual feedback.
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_scrollbar {
            if self.dragging_scrollbar_master {
                self.master_scroll.drag_to_scroll(pos.y);
            } else {
                self.layer_scroll.drag_to_scroll(pos.y);
            }
            self.update_scrollbar(tree);
            return vec![PanelAction::InspectorScrolled(0.0)];
        }

        if let Some(target) = self.pressed_target {
            match target {
                PressedTarget::Macros => self.macros_panel.handle_drag(pos.x, tree),
                PressedTarget::MasterChrome => self.master_chrome.handle_drag(pos, tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag(pos, tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag(pos, tree),
                PressedTarget::MasterEffect(i) => self
                    .master_effects
                    .get_mut(i)
                    .map(|c| c.handle_drag(pos, tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self
                    .layer_effects
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
                PressedTarget::MasterChrome => self.master_chrome.handle_drag_end(tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag_end(tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag_end(tree),
                PressedTarget::MasterEffect(i) => self
                    .master_effects
                    .get_mut(i)
                    .map(|c| c.handle_drag_end(tree))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self
                    .layer_effects
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
    /// Called from ui_root.rs on DragBegin (needs &mut UITree).
    pub fn try_begin_card_drag(&mut self, node_id: u32, tree: &mut UITree) -> bool {
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
            let (col_x, col_w) = if self.card_drag_tab == InspectorTab::Master {
                let half = ((self.viewport_rect.width - COLUMN_PAD * 2.0 - 2.0) * 0.5).floor();
                (self.viewport_rect.x + COLUMN_PAD, half)
            } else {
                let half = ((self.viewport_rect.width - COLUMN_PAD * 2.0 - 2.0) * 0.5).floor();
                (self.column_split_x, half)
            };
            let ghost_w = (col_w - 24.0).min(160.0);
            self.card_drag_ghost_id = tree.add_label(
                -1,
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
                    corner_radius: 4.0,
                    ..UIStyle::default()
                },
            ) as i32;
            self.card_drag_indicator_id = tree.add_panel(
                -1,
                col_x + DRAG_INDICATOR_INSET,
                -100.0,
                col_w - DRAG_INDICATOR_INSET * 2.0,
                DRAG_INDICATOR_H,
                UIStyle {
                    bg_color: DRAG_INDICATOR_COLOR,
                    corner_radius: 1.0,
                    ..UIStyle::default()
                },
            ) as i32;

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
        let (col_x, col_w) = if self.card_drag_tab == InspectorTab::Master {
            let half = ((vp.width - COLUMN_PAD * 2.0 - 2.0) * 0.5).floor();
            (vp.x + COLUMN_PAD, half)
        } else {
            let half = ((vp.width - COLUMN_PAD * 2.0 - 2.0) * 0.5).floor();
            (self.column_split_x, half)
        };
        let ghost_w = (col_w - 24.0).min(160.0);

        // Position ghost centered on cursor, clamped to column
        let ghost_x = (pos.x - ghost_w * 0.5).clamp(
            col_x + DRAG_INDICATOR_INSET,
            col_x + col_w - ghost_w - DRAG_INDICATOR_INSET,
        );
        let ghost_y = (pos.y - DRAG_GHOST_H * 0.5).clamp(vp.y, vp.y + vp.height - DRAG_GHOST_H);

        if self.card_drag_ghost_id >= 0 {
            tree.set_bounds(
                self.card_drag_ghost_id as u32,
                Rect::new(ghost_x, ghost_y, ghost_w, DRAG_GHOST_H),
            );
        }

        // Compute target card index from Y position
        let tab = self.card_drag_tab;
        let (target, indicator_y) = {
            let cards = self.cards_for_tab(tab);
            let card_count = cards.len();
            let mut t = card_count; // default: after last card
            for (i, card) in cards.iter().enumerate() {
                let cy = card.card_y();
                let ch = card.compute_height();
                let mid = cy + ch * 0.5;
                if pos.y < mid {
                    t = i;
                    break;
                }
            }
            let iy = if t < card_count {
                cards[t].card_y()
            } else if card_count > 0 {
                let last = &cards[card_count - 1];
                last.card_y() + last.compute_height()
            } else {
                vp.y
            };
            (t, iy)
        };
        self.card_drag_target_index = target;

        if self.card_drag_indicator_id >= 0 {
            tree.set_bounds(
                self.card_drag_indicator_id as u32,
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
                cards.last().unwrap().effect_index() + 1
            } else {
                0
            }
        };

        // Hide ghost + indicator (move offscreen)
        if self.card_drag_ghost_id >= 0 {
            tree.set_bounds(
                self.card_drag_ghost_id as u32,
                Rect::new(0.0, -100.0, 0.0, 0.0),
            );
        }
        if self.card_drag_indicator_id >= 0 {
            tree.set_bounds(
                self.card_drag_indicator_id as u32,
                Rect::new(0.0, -100.0, 0.0, 0.0),
            );
        }

        self.card_drag_active = false;
        self.card_drag_ghost_id = -1;
        self.card_drag_indicator_id = -1;

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
    fn find_drag_handle(&self, node_id: u32) -> Option<(InspectorTab, usize, usize, String)> {
        if self.master_visible {
            for (i, card) in self.master_effects.iter().enumerate() {
                if card.is_drag_handle(node_id) {
                    return Some((
                        InspectorTab::Master,
                        i,
                        card.effect_index(),
                        card.effect_name().to_string(),
                    ));
                }
            }
        }
        if self.layer_visible {
            for (i, card) in self.layer_effects.iter().enumerate() {
                if card.is_drag_handle(node_id) {
                    return Some((
                        InspectorTab::Layer,
                        i,
                        card.effect_index(),
                        card.effect_name().to_string(),
                    ));
                }
            }
        }
        None
    }

    fn cards_for_tab(&self, tab: InspectorTab) -> &[ParamCardPanel] {
        match tab {
            InspectorTab::Master => &self.master_effects,
            InspectorTab::Layer | InspectorTab::Clip => &self.layer_effects,
        }
    }

    fn cards_for_tab_mut(&mut self, tab: InspectorTab) -> &mut Vec<ParamCardPanel> {
        match tab {
            InspectorTab::Master => &mut self.master_effects,
            InspectorTab::Layer | InspectorTab::Clip => &mut self.layer_effects,
        }
    }

    // ── Internal event routing ───────────────────────────────────

    /// Check if an effect target is already part of the current selection.
    fn is_effect_target_selected(&self, target: &PressedTarget) -> bool {
        match *target {
            PressedTarget::MasterEffect(i) => self
                .master_effects
                .get(i)
                .is_some_and(|c| self.selected_master_ids.contains(c.effect_id())),
            PressedTarget::LayerEffect(i) => self
                .layer_effects
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

    fn route_click(&mut self, node_id: u32, modifiers: Modifiers) -> Vec<PanelAction> {
        let id = node_id as i32;
        // Add Effect buttons
        if id == self.add_master_effect_btn && id >= 0 {
            return vec![PanelAction::AddEffectClicked(InspectorTab::Master)];
        }
        if id == self.add_layer_effect_btn && id >= 0 {
            return vec![PanelAction::AddEffectClicked(InspectorTab::Layer)];
        }
        if let Some(target) = self.find_target_for_node(node_id) {
            self.update_last_effect_tab(&target);
            match target {
                PressedTarget::Macros => self.macros_panel.handle_click(node_id),
                PressedTarget::MasterChrome => self.master_chrome.handle_click(node_id),
                PressedTarget::LayerChrome => self.layer_chrome.handle_click(node_id),
                PressedTarget::ClipChrome => self.clip_chrome.handle_click(node_id),
                PressedTarget::MasterEffect(i) => {
                    let mut actions = self
                        .master_effects
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id))
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
                    let ei = self
                        .master_effects
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
                    let mut actions = self
                        .layer_effects
                        .get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default();

                    if actions
                        .iter()
                        .any(|a| matches!(a, PanelAction::EffectCardClicked(_)))
                    {
                        self.on_effect_card_clicked(InspectorTab::Layer, i, modifiers);
                    } else if !self.is_effect_target_selected(&PressedTarget::LayerEffect(i)) {
                        self.auto_select_effect(&PressedTarget::LayerEffect(i));
                    }
                    let ei = self
                        .layer_effects
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
                    .map(|gp| gp.handle_click(node_id))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    fn route_pointer_down(
        &mut self,
        node_id: u32,
        pos: Vec2,
        modifiers: Modifiers,
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
                    self.master_effects
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0),
                )),
                PressedTarget::LayerEffect(i) => Some(PanelAction::EffectCardClicked(
                    self.layer_effects
                        .get(i)
                        .map(|c| c.effect_index())
                        .unwrap_or(0),
                )),
                _ => None,
            };

            let mut actions = match target {
                PressedTarget::Macros => self.macros_panel.handle_press(node_id, pos.x),
                PressedTarget::MasterChrome => self.master_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::LayerChrome => self.layer_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::ClipChrome => self.clip_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::MasterEffect(i) => self
                    .master_effects
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos))
                    .unwrap_or_default(),
                PressedTarget::LayerEffect(i) => self
                    .layer_effects
                    .get_mut(i)
                    .map(|c| c.handle_pointer_down(node_id, pos))
                    .unwrap_or_default(),
                PressedTarget::GenParam => self
                    .gen_params
                    .as_mut()
                    .map(|gp| gp.handle_pointer_down(node_id, pos))
                    .unwrap_or_default(),
                PressedTarget::Scrollbar => {
                    self.dragging_scrollbar = true;
                    let id = node_id as i32;
                    self.dragging_scrollbar_master =
                        id == self.master_scroll.track_id() || id == self.master_scroll.thumb_id();
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

}

impl Panel for InspectorCompositePanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.cache_first_node = tree.count();

        let rect = layout.inspector();
        if rect.width <= 0.0 {
            return;
        }

        self.viewport_rect = rect;
        let col_gap = 2.0_f32;
        let total_pad = COLUMN_PAD * 2.0 + col_gap; // left pad + right pad + gap
        let half_w = ((rect.width - total_pad) * 0.5).floor();
        let left_x = rect.x + COLUMN_PAD;
        let right_x = left_x + half_w + col_gap;
        let left_content_w = half_w - SCROLLBAR_W;
        let right_content_w = half_w - SCROLLBAR_W;
        self.column_split_x = right_x;

        // Background panel
        self.bg_panel_id = tree.add_panel(
            -1,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                ..UIStyle::default()
            },
        ) as i32;

        // Macros height for column offset — panel built AFTER columns for z-order
        let macros_h = self.macros_panel.height();
        let columns_y = rect.y + macros_h + 2.0; // 2px gap
        let columns_h = rect.height - macros_h - 2.0;
        self.columns_y = columns_y;
        self.columns_height = columns_h;

        // ── LEFT COLUMN: Master FX ──────────────────────────────────
        let left_clip_rect = Rect::new(left_x, columns_y, half_w, columns_h);
        self.master_scroll.begin(tree, left_clip_rect);
        let left_start = tree.count();

        {
            let mut cy = self.master_scroll.content_y(0.0);
            if self.master_visible {
                let section_h = self.master_column_height();
                tree.add_panel(
                    -1,
                    left_x,
                    cy,
                    left_content_w,
                    section_h,
                    UIStyle {
                        bg_color: SECTION_CARD_BORDER,
                        corner_radius: SECTION_CARD_RADIUS,
                        ..UIStyle::default()
                    },
                );
                tree.add_panel(
                    -1,
                    left_x + 1.0,
                    cy + 1.0,
                    left_content_w - 2.0,
                    section_h - 2.0,
                    UIStyle {
                        bg_color: SECTION_CARD_BG,
                        corner_radius: SECTION_CARD_RADIUS - 1.0,
                        ..UIStyle::default()
                    },
                );
                cy += SECTION_CARD_PAD;

                let inner_x = left_x + SECTION_INSET;
                let inner_w = left_content_w - SECTION_INSET * 2.0;

                let chrome_h = self.master_chrome.compute_height();
                self.master_chrome
                    .build(tree, Rect::new(inner_x, cy, inner_w, chrome_h));
                cy += chrome_h;

                if !self.master_chrome.is_collapsed() {
                    for card in &mut self.master_effects {
                        let card_h = card.compute_height();
                        card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                        cy += card_h + SECTION_GAP;
                    }
                    self.add_master_effect_btn = tree.add_button(
                        -1,
                        inner_x,
                        cy,
                        inner_w,
                        ADD_EFFECT_BTN_H,
                        UIStyle {
                            bg_color: ADD_EFFECT_BTN_BG,
                            hover_bg_color: ADD_EFFECT_BTN_HOVER,
                            text_color: ADD_EFFECT_BTN_TEXT,
                            corner_radius: 4.0,
                            text_align: TextAlign::Center,
                            font_size: color::FONT_LABEL,
                            ..UIStyle::default()
                        },
                        "+ Add Effect",
                    ) as i32;
                }
            }
        }
        self.master_scroll.reparent_content(tree, left_start);
        self.master_scroll
            .build_scrollbar(tree, left_x + left_content_w, &SCROLLBAR_STYLE);

        // ── RIGHT COLUMN: Layer + Clip ──────────────────────────────
        let right_clip_rect = Rect::new(right_x, columns_y, half_w, columns_h);
        self.layer_scroll.begin(tree, right_clip_rect);
        let right_start = tree.count();

        {
            let mut cy = self.layer_scroll.content_y(0.0);

            // Layer section — includes gen params above layer effects
            if self.layer_visible {
                let section_h = self.layer_column_height();
                tree.add_panel(
                    -1,
                    right_x,
                    cy,
                    right_content_w,
                    section_h,
                    UIStyle {
                        bg_color: SECTION_CARD_BORDER,
                        corner_radius: SECTION_CARD_RADIUS,
                        ..UIStyle::default()
                    },
                );
                tree.add_panel(
                    -1,
                    right_x + 1.0,
                    cy + 1.0,
                    right_content_w - 2.0,
                    section_h - 2.0,
                    UIStyle {
                        bg_color: SECTION_CARD_BG,
                        corner_radius: SECTION_CARD_RADIUS - 1.0,
                        ..UIStyle::default()
                    },
                );
                cy += SECTION_CARD_PAD;

                let inner_x = right_x + SECTION_INSET;
                let inner_w = right_content_w - SECTION_INSET * 2.0;

                let chrome_h = self.layer_chrome.compute_height();
                self.layer_chrome
                    .build(tree, Rect::new(inner_x, cy, inner_w, chrome_h));
                cy += chrome_h;

                if !self.layer_chrome.is_collapsed() {
                    if let Some(ref mut gp) = self.gen_params {
                        let gp_h = gp.compute_height();
                        gp.build(tree, Rect::new(inner_x, cy, inner_w, gp_h));
                        cy += gp_h + SECTION_GAP;
                    }

                    for card in &mut self.layer_effects {
                        let card_h = card.compute_height();
                        card.build(tree, Rect::new(inner_x, cy, inner_w, card_h));
                        cy += card_h + SECTION_GAP;
                    }
                    self.add_layer_effect_btn = tree.add_button(
                        -1,
                        inner_x,
                        cy,
                        inner_w,
                        ADD_EFFECT_BTN_H,
                        UIStyle {
                            bg_color: ADD_EFFECT_BTN_BG,
                            hover_bg_color: ADD_EFFECT_BTN_HOVER,
                            text_color: ADD_EFFECT_BTN_TEXT,
                            corner_radius: 4.0,
                            text_align: TextAlign::Center,
                            font_size: color::FONT_LABEL,
                            ..UIStyle::default()
                        },
                        "+ Add Effect",
                    ) as i32;
                }
            }
        }
        self.layer_scroll.reparent_content(tree, right_start);
        self.layer_scroll
            .build_scrollbar(tree, right_x + right_content_w, &SCROLLBAR_STYLE);

        // ── MACROS STRIP (built last so it renders on top of columns) ──
        let macros_rect = Rect::new(left_x, rect.y, rect.width - COLUMN_PAD * 2.0, macros_h);
        self.macros_panel.build(tree, macros_rect);

        self.update_scroll_bounds();
        self.update_scrollbar(tree);

        self.cache_node_count = tree.count() - self.cache_first_node;
    }

    fn update(&mut self, _tree: &mut UITree) {
        // State sync is done via direct accessors on sub-panels.
        // The app layer calls sync methods like:
        //   inspector.master_chrome_mut().sync_opacity(&mut tree, 0.5);
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click {
                node_id,
                pos,
                modifiers,
            } => {
                if !self.viewport_rect.contains(*pos) {
                    return Vec::new();
                }
                self.route_click(*node_id, *modifiers)
            }
            UIEvent::PointerDown {
                node_id,
                pos,
                modifiers,
            } => {
                if !self.viewport_rect.contains(*pos) {
                    return Vec::new();
                }
                self.route_pointer_down(*node_id, *pos, *modifiers)
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
                if self.pressed_target.is_none() {
                    let target = self.find_target_for_node(*node_id);
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
        for card in &self.master_effects {
            card.register_intents(intents);
        }
        for card in &self.layer_effects {
            card.register_intents(intents);
        }
        if let Some(gp) = self.gen_params.as_ref() {
            gp.register_intents(intents);
        }
        self.macros_panel.register_intents(intents);
        self.master_chrome.register_intents(intents);
        self.layer_chrome.register_intents(intents);
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
    use crate::tree::UITree;

    fn inspector_layout() -> ScreenLayout {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 280.0;
        layout
    }

    #[test]
    fn build_empty_inspector() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        assert!(panel.bg_panel_id >= 0);
        assert!(panel.layer_scroll.track_id() >= 0);
        assert!(panel.layer_scroll.thumb_id() >= 0);
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
        assert_eq!(panel.bg_panel_id, -1);
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
    fn section_visibility() {
        let mut panel = InspectorCompositePanel::new();
        panel.set_section_visible(InspectorTab::Master, false);
        panel.set_section_visible(InspectorTab::Layer, false);

        assert!(!panel.master_visible);
        assert!(!panel.layer_visible);
        assert!(panel.clip_visible);
    }

    #[test]
    fn find_target_for_scrollbar() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        let target = panel.find_target_for_node(panel.layer_scroll.track_id() as u32);
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
            let target = panel.find_target_for_node(first as u32);
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
        let actions =
            panel.route_click(panel.master_chrome.first_node() as u32 + 1, Modifiers::NONE);
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

        // Simulate scrollbar pointer down
        let sb_id = panel.layer_scroll.thumb_id() as u32;
        let pos = Vec2::new(280.0, 100.0);
        panel.route_pointer_down(sb_id, pos, crate::input::Modifiers::NONE);

        assert!(panel.is_dragging());
        assert!(panel.dragging_scrollbar);
    }
}
