use std::collections::HashSet;
use manifold_core::LayerId;
use crate::color;
use crate::input::{Modifiers, UIEvent};
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction, InspectorTab};
use super::master_chrome::MasterChromePanel;
use super::layer_chrome::LayerChromePanel;
use super::clip_chrome::ClipChromePanel;
use super::effect_card::{EffectCardPanel, EffectCardConfig};
use super::gen_param::{GenParamPanel, GenParamConfig};

// ── Layout constants ────────────────────────────────────────────

const SCROLLBAR_W: f32 = 4.0;
const SCROLLBAR_MIN_THUMB_H: f32 = 16.0;
const SCROLL_SPEED: f32 = 1.0;
const SECTION_GAP: f32 = 2.0;

const SCROLLBAR_TRACK_COLOR: Color32 = color::SCROLLBAR_TRACK_C32;
const SCROLLBAR_THUMB_COLOR: Color32 = color::SCROLLBAR_THUMB_C32;
const SCROLLBAR_THUMB_HOVER: Color32 = color::SCROLLBAR_THUMB_HOVER_C32;

const ADD_EFFECT_BTN_H: f32 = 26.0;

// ── Effect card drag-reorder constants (Unity EffectsListBitmapPanel) ──
const DRAG_GHOST_H: f32 = 24.0;
const DRAG_GHOST_FONT_SIZE: u16 = 10;
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
    MasterChrome,
    LayerChrome,
    ClipChrome,
    MasterEffect(usize),
    LayerEffect(usize),
    ClipEffect(usize),
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
    master_chrome: MasterChromePanel,
    layer_chrome: LayerChromePanel,
    clip_chrome: ClipChromePanel,
    master_effects: Vec<EffectCardPanel>,
    layer_effects: Vec<EffectCardPanel>,
    clip_effects: Vec<EffectCardPanel>,
    gen_params: Option<GenParamPanel>,

    // Section visibility
    master_visible: bool,
    layer_visible: bool,
    clip_visible: bool,

    // Add Effect button node IDs
    add_master_effect_btn: i32,
    add_layer_effect_btn: i32,
    add_clip_effect_btn: i32,

    // Scroll state
    scroll_offset: f32,
    max_scroll: f32,
    content_height: f32,
    viewport_rect: Rect,

    // Scrollbar node IDs
    scrollbar_track_id: i32,
    scrollbar_thumb_id: i32,
    dragging_scrollbar: bool,

    // Event routing
    pressed_target: Option<PressedTarget>,
    /// Remembers which inspector tab (Master/Layer/Clip) the last effect
    /// interaction targeted. Survives across drag_end so dispatch can
    /// route effect actions to the correct data location.
    last_effect_tab: InspectorTab,

    // Background
    bg_panel_id: i32,

    // ── Effect selection state (Unity EffectSelectionManager — per tab) ──
    selected_master_indices: HashSet<usize>,
    selected_layer_indices: HashSet<usize>,
    selected_clip_indices: HashSet<usize>,
    last_clicked_master: i32,
    last_clicked_layer: i32,
    last_clicked_clip: i32,

    // ── Effect card drag-reorder state (Unity EffectsListBitmapPanel) ──
    card_drag_active: bool,
    card_drag_tab: InspectorTab,
    card_drag_source_index: usize, // index within the tab's effect cards vec
    card_drag_effect_index: usize, // effect_index in the flat effects list
    card_drag_target_index: usize, // current drop target index
    card_drag_ghost_id: i32,
    card_drag_indicator_id: i32,
    card_drag_label: String,
}

impl InspectorCompositePanel {
    pub fn new() -> Self {
        Self {
            master_chrome: MasterChromePanel::new(),
            layer_chrome: LayerChromePanel::new(),
            clip_chrome: ClipChromePanel::new(),
            master_effects: Vec::new(),
            layer_effects: Vec::new(),
            clip_effects: Vec::new(),
            gen_params: None,
            master_visible: true,
            layer_visible: true,
            clip_visible: true,
            add_master_effect_btn: -1,
            add_layer_effect_btn: -1,
            add_clip_effect_btn: -1,
            scroll_offset: 0.0,
            max_scroll: 0.0,
            content_height: 0.0,
            viewport_rect: Rect::ZERO,
            scrollbar_track_id: -1,
            scrollbar_thumb_id: -1,
            dragging_scrollbar: false,
            pressed_target: None,
            last_effect_tab: InspectorTab::Layer,
            bg_panel_id: -1,
            selected_master_indices: HashSet::new(),
            selected_layer_indices: HashSet::new(),
            selected_clip_indices: HashSet::new(),
            last_clicked_master: -1,
            last_clicked_layer: -1,
            last_clicked_clip: -1,
            card_drag_active: false,
            card_drag_tab: InspectorTab::Master,
            card_drag_source_index: 0,
            card_drag_effect_index: 0,
            card_drag_target_index: 0,
            card_drag_ghost_id: -1,
            card_drag_indicator_id: -1,
            card_drag_label: String::new(),
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

    pub fn configure_master_effects(&mut self, configs: &[EffectCardConfig]) {
        self.master_effects.clear();
        for cfg in configs {
            let mut card = EffectCardPanel::new();
            card.configure(cfg);
            self.master_effects.push(card);
        }
    }

    pub fn configure_layer_effects(&mut self, configs: &[EffectCardConfig]) {
        self.layer_effects.clear();
        for cfg in configs {
            let mut card = EffectCardPanel::new();
            card.configure(cfg);
            self.layer_effects.push(card);
        }
    }

    pub fn configure_clip_effects(&mut self, configs: &[EffectCardConfig]) {
        self.clip_effects.clear();
        for cfg in configs {
            let mut card = EffectCardPanel::new();
            card.configure(cfg);
            self.clip_effects.push(card);
        }
    }

    pub fn configure_gen_params(&mut self, config: Option<&GenParamConfig>, layer_id: Option<LayerId>) {
        if let Some(cfg) = config {
            let mut panel = GenParamPanel::new();
            panel.set_layer_id(layer_id);
            panel.configure(cfg);
            self.gen_params = Some(panel);
        } else {
            self.gen_params = None;
        }
    }

    // ── Accessors ─────────────────────────────────────────────────

    pub fn master_chrome(&self) -> &MasterChromePanel { &self.master_chrome }
    pub fn master_chrome_mut(&mut self) -> &mut MasterChromePanel { &mut self.master_chrome }
    pub fn layer_chrome(&self) -> &LayerChromePanel { &self.layer_chrome }
    pub fn layer_chrome_mut(&mut self) -> &mut LayerChromePanel { &mut self.layer_chrome }
    pub fn clip_chrome(&self) -> &ClipChromePanel { &self.clip_chrome }
    pub fn clip_chrome_mut(&mut self) -> &mut ClipChromePanel { &mut self.clip_chrome }
    pub fn gen_params_mut(&mut self) -> Option<&mut GenParamPanel> { self.gen_params.as_mut() }

    pub fn master_effect_mut(&mut self, idx: usize) -> Option<&mut EffectCardPanel> {
        self.master_effects.get_mut(idx)
    }
    pub fn layer_effect_mut(&mut self, idx: usize) -> Option<&mut EffectCardPanel> {
        self.layer_effects.get_mut(idx)
    }
    pub fn clip_effect_mut(&mut self, idx: usize) -> Option<&mut EffectCardPanel> {
        self.clip_effects.get_mut(idx)
    }

    pub fn viewport_rect(&self) -> Rect { self.viewport_rect }
    pub fn scroll_offset(&self) -> f32 { self.scroll_offset }
    /// Which inspector tab the last effect interaction targeted.
    /// Dispatch uses this to route EffectParamChanged etc. to the
    /// correct data location (master / layer / clip effects).
    pub fn last_effect_tab(&self) -> InspectorTab { self.last_effect_tab }

    pub fn is_dragging(&self) -> bool {
        self.dragging_scrollbar
            || self.card_drag_active
            || self.master_chrome.is_dragging()
            || self.layer_chrome.is_dragging()
            || self.clip_chrome.is_dragging()
            || self.master_effects.iter().any(|e| e.is_dragging())
            || self.layer_effects.iter().any(|e| e.is_dragging())
            || self.clip_effects.iter().any(|e| e.is_dragging())
            || self.gen_params.as_ref().is_some_and(|p| p.is_dragging())
    }

    // ── Scrolling ─────────────────────────────────────────────────

    /// Call on mouse wheel within the inspector viewport.
    /// Positive delta scrolls down.
    pub fn handle_scroll(&mut self, delta: f32) {
        self.scroll_offset = (self.scroll_offset - delta * SCROLL_SPEED)
            .clamp(0.0, self.max_scroll);
    }

    fn compute_content_height(&self) -> f32 {
        let mut h = 0.0;

        if self.master_visible {
            h += self.master_chrome.compute_height();
            if !self.master_chrome.is_collapsed() {
                for card in &self.master_effects {
                    h += card.compute_height() + SECTION_GAP;
                }
                h += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            h += SECTION_GAP;
        }

        if self.layer_visible {
            h += self.layer_chrome.compute_height();
            if !self.layer_chrome.is_collapsed() {
                for card in &self.layer_effects {
                    h += card.compute_height() + SECTION_GAP;
                }
                h += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            h += SECTION_GAP;
        }

        if self.clip_visible {
            h += self.clip_chrome.compute_height();
            if !self.clip_chrome.is_collapsed() {
                if let Some(ref gp) = self.gen_params {
                    h += gp.compute_height() + SECTION_GAP;
                }
                for card in &self.clip_effects {
                    h += card.compute_height() + SECTION_GAP;
                }
                h += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            h += SECTION_GAP;
        }

        h
    }

    fn update_scroll_bounds(&mut self) {
        self.content_height = self.compute_content_height();
        self.max_scroll = (self.content_height - self.viewport_rect.height).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(0.0, self.max_scroll);
    }

    fn update_scrollbar(&self, tree: &mut UITree) {
        if self.scrollbar_track_id < 0 { return; }

        let vp_h = self.viewport_rect.height;
        if self.content_height <= vp_h || vp_h <= 0.0 {
            tree.set_visible(self.scrollbar_track_id as u32, false);
            tree.set_visible(self.scrollbar_thumb_id as u32, false);
            return;
        }

        tree.set_visible(self.scrollbar_track_id as u32, true);
        tree.set_visible(self.scrollbar_thumb_id as u32, true);

        let ratio = vp_h / self.content_height;
        let thumb_h = (vp_h * ratio).max(SCROLLBAR_MIN_THUMB_H);
        let scroll_range = vp_h - thumb_h;
        let scroll_frac = if self.max_scroll > 0.0 {
            self.scroll_offset / self.max_scroll
        } else {
            0.0
        };

        let thumb_x = self.viewport_rect.x + self.viewport_rect.width - SCROLLBAR_W;
        let thumb_y = self.viewport_rect.y + scroll_frac * scroll_range;
        tree.set_bounds(
            self.scrollbar_thumb_id as u32,
            Rect::new(thumb_x, thumb_y, SCROLLBAR_W, thumb_h),
        );
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
            PressedTarget::ClipChrome | PressedTarget::ClipEffect(_) | PressedTarget::GenParam => {
                self.last_effect_tab = InspectorTab::Clip;
            }
            PressedTarget::Scrollbar => {}
        }
    }

    // ── Effect selection (Unity EffectSelectionManager) ─────────

    /// Get the selection set and cards vec for a given tab.
    fn selection_for_tab(&self, tab: InspectorTab) -> (&HashSet<usize>, &[EffectCardPanel]) {
        match tab {
            InspectorTab::Master => (&self.selected_master_indices, &self.master_effects),
            InspectorTab::Layer => (&self.selected_layer_indices, &self.layer_effects),
            InspectorTab::Clip => (&self.selected_clip_indices, &self.clip_effects),
        }
    }

    fn last_clicked_for_tab(&self, tab: InspectorTab) -> i32 {
        match tab {
            InspectorTab::Master => self.last_clicked_master,
            InspectorTab::Layer => self.last_clicked_layer,
            InspectorTab::Clip => self.last_clicked_clip,
        }
    }

    fn set_last_clicked_for_tab(&mut self, tab: InspectorTab, idx: i32) {
        match tab {
            InspectorTab::Master => self.last_clicked_master = idx,
            InspectorTab::Layer => self.last_clicked_layer = idx,
            InspectorTab::Clip => self.last_clicked_clip = idx,
        }
    }

    fn selection_set_mut(&mut self, tab: InspectorTab) -> &mut HashSet<usize> {
        match tab {
            InspectorTab::Master => &mut self.selected_master_indices,
            InspectorTab::Layer => &mut self.selected_layer_indices,
            InspectorTab::Clip => &mut self.selected_clip_indices,
        }
    }

    /// Unity EffectSelectionManager.OnCardClicked (lines 164-177)
    /// Dispatches to select/toggle/range based on modifiers.
    fn on_effect_card_clicked(&mut self, tab: InspectorTab, card_index: usize, modifiers: Modifiers) {
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
    /// Select a single card, clearing all others in this tab.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn select_effect(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() { return; }

        let set = self.selection_set_mut(tab);
        set.clear();
        set.insert(card_index);
        self.set_last_clicked_for_tab(tab, card_index as i32);
    }

    /// Unity EffectSelectionManager.ToggleCardSelection (lines 103-118)
    /// Cmd+Click: toggle in/out of multi-selection.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn toggle_effect_selection(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() { return; }

        let set = self.selection_set_mut(tab);
        if set.contains(&card_index) {
            set.remove(&card_index);
        } else {
            set.insert(card_index);
        }
        self.set_last_clicked_for_tab(tab, card_index as i32);
    }

    /// Unity EffectSelectionManager.RangeSelectCards (lines 121-139)
    /// Shift+Click: range select from last clicked anchor to this index.
    /// Note: does NOT update card visuals — call apply_selection_visuals() after.
    fn range_select_effects(&mut self, tab: InspectorTab, card_index: usize) {
        let cards = self.cards_for_tab(tab);
        if card_index >= cards.len() { return; }

        let anchor = self.last_clicked_for_tab(tab);
        let anchor = if anchor >= 0 { anchor as usize } else { 0 };

        let lo = anchor.min(card_index);
        let hi = anchor.max(card_index);

        let set = self.selection_set_mut(tab);
        set.clear();
        for i in lo..=hi {
            set.insert(i);
        }
        // Keep lastClickedIndex as anchor — do not update (Unity line 138)
    }

    /// Clear all effect selection across all tabs.
    /// Unity EffectSelectionManager.ClearSelection (lines 141-146)
    pub fn clear_effect_selection(&mut self) {
        for tab in [InspectorTab::Master, InspectorTab::Layer, InspectorTab::Clip] {
            self.selection_set_mut(tab).clear();
            self.set_last_clicked_for_tab(tab, -1);
        }
        // Reset is_selected on all cards (visuals deferred to rebuild)
        for card in self.master_effects.iter_mut()
            .chain(self.layer_effects.iter_mut())
            .chain(self.clip_effects.iter_mut())
        {
            card.set_selected(false);
        }
    }

    /// Apply selection visuals to the tree (call after handle_event when
    /// EffectCardClicked is returned). Updates border colors without rebuild.
    /// This is the SINGLE place that syncs is_selected + tree style together.
    pub fn apply_selection_visuals(&mut self, tree: &mut UITree) {
        for tab in [InspectorTab::Master, InspectorTab::Layer, InspectorTab::Clip] {
            let (set, _) = self.selection_for_tab(tab);
            let set_clone: Vec<usize> = set.iter().copied().collect();
            let cards = self.cards_for_tab_mut(tab);
            for (i, card) in cards.iter_mut().enumerate() {
                let selected = set_clone.contains(&i);
                card.update_selection_visual(tree, selected);
            }
        }
    }

    /// Whether any effects are selected (for keyboard shortcut routing).
    pub fn has_effect_selection(&self) -> bool {
        !self.selected_master_indices.is_empty()
            || !self.selected_layer_indices.is_empty()
            || !self.selected_clip_indices.is_empty()
    }

    /// Get all selected effect indices for the active tab.
    /// Returns sorted ascending (Unity: GetSelectedIndices).
    pub fn get_selected_effect_indices(&self) -> Vec<usize> {
        let (set, _) = self.selection_for_tab(self.last_effect_tab);
        let mut indices: Vec<usize> = set.iter().copied().collect();
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

        // Scrollbar
        if id == self.scrollbar_track_id || id == self.scrollbar_thumb_id {
            return Some(PressedTarget::Scrollbar);
        }

        // Master section
        if self.master_visible {
            if in_range(idx, self.master_chrome.first_node(), self.master_chrome.node_count()) {
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
            if in_range(idx, self.layer_chrome.first_node(), self.layer_chrome.node_count()) {
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
            if in_range(idx, self.clip_chrome.first_node(), self.clip_chrome.node_count()) {
                return Some(PressedTarget::ClipChrome);
            }
            if let Some(ref gp) = self.gen_params
                && in_range(idx, gp.first_node(), gp.node_count()) {
                    return Some(PressedTarget::GenParam);
                }
            for (i, card) in self.clip_effects.iter().enumerate() {
                if in_range(idx, card.first_node(), card.node_count()) {
                    return Some(PressedTarget::ClipEffect(i));
                }
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

    /// Route drag events to the pressed sub-panel.
    /// Called from UIRoot::process_events (not through Panel::handle_event)
    /// because it needs &mut UITree for slider visual feedback.
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if self.dragging_scrollbar {
            let vp = self.viewport_rect;
            let ratio = vp.height / self.content_height;
            let thumb_h = (vp.height * ratio).max(SCROLLBAR_MIN_THUMB_H);
            let scroll_range = vp.height - thumb_h;
            if scroll_range > 0.0 {
                let frac = ((pos.y - vp.y) / scroll_range).clamp(0.0, 1.0);
                self.scroll_offset = frac * self.max_scroll;
                self.update_scrollbar(tree);
            }
            return vec![PanelAction::InspectorScrolled(self.scroll_offset)];
        }

        if let Some(target) = self.pressed_target {
            match target {
                PressedTarget::MasterChrome => self.master_chrome.handle_drag(pos, tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag(pos, tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag(pos, tree),
                PressedTarget::MasterEffect(i) => {
                    self.master_effects.get_mut(i)
                        .map(|c| c.handle_drag(pos, tree))
                        .unwrap_or_default()
                }
                PressedTarget::LayerEffect(i) => {
                    self.layer_effects.get_mut(i)
                        .map(|c| c.handle_drag(pos, tree))
                        .unwrap_or_default()
                }
                PressedTarget::ClipEffect(i) => {
                    self.clip_effects.get_mut(i)
                        .map(|c| c.handle_drag(pos, tree))
                        .unwrap_or_default()
                }
                PressedTarget::GenParam => {
                    self.gen_params.as_mut()
                        .map(|gp| gp.handle_drag(pos, tree))
                        .unwrap_or_default()
                }
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
                PressedTarget::MasterChrome => self.master_chrome.handle_drag_end(tree),
                PressedTarget::LayerChrome => self.layer_chrome.handle_drag_end(tree),
                PressedTarget::ClipChrome => self.clip_chrome.handle_drag_end(tree),
                PressedTarget::MasterEffect(i) => {
                    self.master_effects.get_mut(i)
                        .map(|c| c.handle_drag_end(tree))
                        .unwrap_or_default()
                }
                PressedTarget::LayerEffect(i) => {
                    self.layer_effects.get_mut(i)
                        .map(|c| c.handle_drag_end(tree))
                        .unwrap_or_default()
                }
                PressedTarget::ClipEffect(i) => {
                    self.clip_effects.get_mut(i)
                        .map(|c| c.handle_drag_end(tree))
                        .unwrap_or_default()
                }
                PressedTarget::GenParam => {
                    self.gen_params.as_mut()
                        .map(|gp| gp.handle_drag_end(tree))
                        .unwrap_or_default()
                }
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

            // Dim source card border (Unity: SetDragDimmed(true))
            let cards = self.cards_for_tab_mut(tab);
            if let Some(card) = cards.get(card_idx) {
                card.set_drag_dimmed(tree, true);
            }

            // Create ghost + indicator nodes
            let panel_w = self.viewport_rect.width;
            let ghost_w = (panel_w - 24.0).min(160.0);
            self.card_drag_ghost_id = tree.add_label(
                -1, 0.0, -100.0, ghost_w, DRAG_GHOST_H,
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
                -1, self.viewport_rect.x + DRAG_INDICATOR_INSET, -100.0,
                panel_w - DRAG_INDICATOR_INSET * 2.0, DRAG_INDICATOR_H,
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
        if !self.card_drag_active { return; }

        let vp = self.viewport_rect;
        let panel_w = vp.width;
        let ghost_w = (panel_w - 24.0).min(160.0);

        // Position ghost centered on cursor, clamped to viewport
        let ghost_x = (pos.x - ghost_w * 0.5).clamp(
            vp.x + DRAG_INDICATOR_INSET,
            vp.x + panel_w - ghost_w - DRAG_INDICATOR_INSET,
        );
        let ghost_y = (pos.y - DRAG_GHOST_H * 0.5).clamp(vp.y, vp.y + vp.height - DRAG_GHOST_H);

        if self.card_drag_ghost_id >= 0 {
            tree.set_bounds(self.card_drag_ghost_id as u32,
                Rect::new(ghost_x, ghost_y, ghost_w, DRAG_GHOST_H));
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
            tree.set_bounds(self.card_drag_indicator_id as u32,
                Rect::new(
                    vp.x + DRAG_INDICATOR_INSET,
                    indicator_y - DRAG_INDICATOR_H * 0.5,
                    panel_w - DRAG_INDICATOR_INSET * 2.0,
                    DRAG_INDICATOR_H,
                ));
        }
    }

    /// End card drag — restore dimming, hide ghost/indicator, return reorder action.
    pub fn end_card_drag(&mut self, tree: &mut UITree) -> Vec<PanelAction> {
        if !self.card_drag_active { return Vec::new(); }

        let src = self.card_drag_source_index;
        let tab = self.card_drag_tab;
        let from = self.card_drag_effect_index;
        let to_card = self.card_drag_target_index;

        // Restore source card border + compute target effect index
        // (scope borrow of cards before mutating self fields)
        let to_fx = {
            let cards = self.cards_for_tab(tab);
            if let Some(card) = cards.get(src) {
                card.set_drag_dimmed(tree, false);
            }
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
            tree.set_bounds(self.card_drag_ghost_id as u32, Rect::new(0.0, -100.0, 0.0, 0.0));
        }
        if self.card_drag_indicator_id >= 0 {
            tree.set_bounds(self.card_drag_indicator_id as u32, Rect::new(0.0, -100.0, 0.0, 0.0));
        }

        self.card_drag_active = false;
        self.card_drag_ghost_id = -1;
        self.card_drag_indicator_id = -1;

        // Only emit action if position actually changed
        if to_fx != from && to_fx != from + 1 {
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
                    return Some((InspectorTab::Master, i, card.effect_index(), card.effect_name().to_string()));
                }
            }
        }
        if self.layer_visible {
            for (i, card) in self.layer_effects.iter().enumerate() {
                if card.is_drag_handle(node_id) {
                    return Some((InspectorTab::Layer, i, card.effect_index(), card.effect_name().to_string()));
                }
            }
        }
        if self.clip_visible {
            for (i, card) in self.clip_effects.iter().enumerate() {
                if card.is_drag_handle(node_id) {
                    return Some((InspectorTab::Clip, i, card.effect_index(), card.effect_name().to_string()));
                }
            }
        }
        None
    }

    fn cards_for_tab(&self, tab: InspectorTab) -> &[EffectCardPanel] {
        match tab {
            InspectorTab::Master => &self.master_effects,
            InspectorTab::Layer => &self.layer_effects,
            InspectorTab::Clip => &self.clip_effects,
        }
    }

    fn cards_for_tab_mut(&mut self, tab: InspectorTab) -> &mut Vec<EffectCardPanel> {
        match tab {
            InspectorTab::Master => &mut self.master_effects,
            InspectorTab::Layer => &mut self.layer_effects,
            InspectorTab::Clip => &mut self.clip_effects,
        }
    }

    // ── Internal event routing ───────────────────────────────────

    /// Auto-select an effect card on any interaction (click, pointer down).
    /// Unity: any card interaction implicitly selects it (single-select, no modifiers).
    fn auto_select_effect(&mut self, target: &PressedTarget) {
        match *target {
            PressedTarget::MasterEffect(i) => self.select_effect(InspectorTab::Master, i),
            PressedTarget::LayerEffect(i) => self.select_effect(InspectorTab::Layer, i),
            PressedTarget::ClipEffect(i) => self.select_effect(InspectorTab::Clip, i),
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
        if id == self.add_clip_effect_btn && id >= 0 {
            return vec![PanelAction::AddEffectClicked(InspectorTab::Clip)];
        }

        if let Some(target) = self.find_target_for_node(node_id) {
            self.update_last_effect_tab(&target);
            match target {
                PressedTarget::MasterChrome => self.master_chrome.handle_click(node_id),
                PressedTarget::LayerChrome => self.layer_chrome.handle_click(node_id),
                PressedTarget::ClipChrome => self.clip_chrome.handle_click(node_id),
                PressedTarget::MasterEffect(i) => {
                    let mut actions = self.master_effects.get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default();
                    // Header click: modifier-aware selection. Everything else: auto-select.
                    if actions.iter().any(|a| matches!(a, PanelAction::EffectCardClicked(_))) {
                        self.on_effect_card_clicked(InspectorTab::Master, i, modifiers);
                    } else {
                        self.auto_select_effect(&PressedTarget::MasterEffect(i));
                        let ei = self.master_effects.get(i).map(|c| c.effect_index()).unwrap_or(0);
                        actions.insert(0, PanelAction::EffectCardClicked(ei));
                    }
                    actions
                }
                PressedTarget::LayerEffect(i) => {
                    let mut actions = self.layer_effects.get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default();
                    if actions.iter().any(|a| matches!(a, PanelAction::EffectCardClicked(_))) {
                        self.on_effect_card_clicked(InspectorTab::Layer, i, modifiers);
                    } else {
                        self.auto_select_effect(&PressedTarget::LayerEffect(i));
                        let ei = self.layer_effects.get(i).map(|c| c.effect_index()).unwrap_or(0);
                        actions.insert(0, PanelAction::EffectCardClicked(ei));
                    }
                    actions
                }
                PressedTarget::ClipEffect(i) => {
                    let mut actions = self.clip_effects.get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default();
                    if actions.iter().any(|a| matches!(a, PanelAction::EffectCardClicked(_))) {
                        self.on_effect_card_clicked(InspectorTab::Clip, i, modifiers);
                    } else {
                        self.auto_select_effect(&PressedTarget::ClipEffect(i));
                        let ei = self.clip_effects.get(i).map(|c| c.effect_index()).unwrap_or(0);
                        actions.insert(0, PanelAction::EffectCardClicked(ei));
                    }
                    actions
                }
                PressedTarget::GenParam => {
                    self.gen_params.as_mut()
                        .map(|gp| gp.handle_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }

    fn route_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        let target = self.find_target_for_node(node_id);
        self.pressed_target = target;
        // Record which tab this interaction targets (survives drag_end)
        if let Some(ref t) = target {
            self.update_last_effect_tab(t);
            // Auto-select on any pointer-down interaction (slider drag, trim, etc.)
            self.auto_select_effect(t);
        }

        if let Some(target) = target {
            // For effect targets, prepend EffectCardClicked to trigger visual update
            let select_action = match target {
                PressedTarget::MasterEffect(i) => {
                    Some(PanelAction::EffectCardClicked(self.master_effects.get(i).map(|c| c.effect_index()).unwrap_or(0)))
                }
                PressedTarget::LayerEffect(i) => {
                    Some(PanelAction::EffectCardClicked(self.layer_effects.get(i).map(|c| c.effect_index()).unwrap_or(0)))
                }
                PressedTarget::ClipEffect(i) => {
                    Some(PanelAction::EffectCardClicked(self.clip_effects.get(i).map(|c| c.effect_index()).unwrap_or(0)))
                }
                _ => None,
            };

            let mut actions = match target {
                PressedTarget::MasterChrome => self.master_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::LayerChrome => self.layer_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::ClipChrome => self.clip_chrome.handle_pointer_down(node_id, pos),
                PressedTarget::MasterEffect(i) => {
                    self.master_effects.get_mut(i)
                        .map(|c| c.handle_pointer_down(node_id, pos))
                        .unwrap_or_default()
                }
                PressedTarget::LayerEffect(i) => {
                    self.layer_effects.get_mut(i)
                        .map(|c| c.handle_pointer_down(node_id, pos))
                        .unwrap_or_default()
                }
                PressedTarget::ClipEffect(i) => {
                    self.clip_effects.get_mut(i)
                        .map(|c| c.handle_pointer_down(node_id, pos))
                        .unwrap_or_default()
                }
                PressedTarget::GenParam => {
                    self.gen_params.as_mut()
                        .map(|gp| gp.handle_pointer_down(node_id, pos))
                        .unwrap_or_default()
                }
                PressedTarget::Scrollbar => {
                    self.dragging_scrollbar = true;
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

    fn route_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        if let Some(target) = self.find_target_for_node(node_id) {
            match target {
                PressedTarget::MasterChrome => self.master_chrome.handle_right_click(node_id),
                PressedTarget::LayerChrome => self.layer_chrome.handle_right_click(node_id),
                PressedTarget::ClipChrome => self.clip_chrome.handle_right_click(node_id),
                PressedTarget::MasterEffect(i) => {
                    self.master_effects.get(i)
                        .map(|c| c.handle_right_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::LayerEffect(i) => {
                    self.layer_effects.get(i)
                        .map(|c| c.handle_right_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::ClipEffect(i) => {
                    self.clip_effects.get(i)
                        .map(|c| c.handle_right_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::GenParam => {
                    self.gen_params.as_ref()
                        .map(|gp| gp.handle_right_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::Scrollbar => Vec::new(),
            }
        } else {
            Vec::new()
        }
    }
}

impl Panel for InspectorCompositePanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        let rect = layout.inspector();
        if rect.width <= 0.0 {
            return;
        }

        self.viewport_rect = rect;
        let content_w = rect.width - SCROLLBAR_W;

        // Background panel
        self.bg_panel_id = tree.add_panel(
            -1, rect.x, rect.y, rect.width, rect.height,
            UIStyle { bg_color: color::INSPECTOR_BG, ..UIStyle::default() },
        ) as i32;

        // ClipRegion — clips all scrolled content to the inspector viewport.
        // Unity: InspectorCompositeBitmapPanel uses a viewport-sized RT for natural clipping.
        // Rust equivalent: a ClipRegion node with CLIPS_CHILDREN flag.
        let clip_id = tree.add_node(
            -1,
            rect,
            crate::node::UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        ) as i32;

        // Track the start index so we can reparent all content nodes under the clip region
        let content_start = tree.count();

        let mut cy = rect.y - self.scroll_offset;

        // Master section
        if self.master_visible {
            let chrome_h = self.master_chrome.compute_height();
            self.master_chrome.build(tree, Rect::new(rect.x, cy, content_w, chrome_h));
            cy += chrome_h;

            if !self.master_chrome.is_collapsed() {
                for card in &mut self.master_effects {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(rect.x, cy, content_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
                // "+ Add Effect" button for master
                self.add_master_effect_btn = tree.add_button(
                    -1, rect.x + 4.0, cy, content_w - 8.0, ADD_EFFECT_BTN_H,
                    UIStyle {
                        bg_color: ADD_EFFECT_BTN_BG,
                        hover_bg_color: ADD_EFFECT_BTN_HOVER,
                        text_color: ADD_EFFECT_BTN_TEXT,
                        corner_radius: 4.0,
                        text_align: TextAlign::Center,
                        font_size: 11,
                        ..UIStyle::default()
                    },
                    "+ Add Effect",
                ) as i32;
                cy += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            cy += SECTION_GAP;
        }

        // Layer section
        if self.layer_visible {
            let chrome_h = self.layer_chrome.compute_height();
            self.layer_chrome.build(tree, Rect::new(rect.x, cy, content_w, chrome_h));
            cy += chrome_h;

            if !self.layer_chrome.is_collapsed() {
                for card in &mut self.layer_effects {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(rect.x, cy, content_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
                // "+ Add Effect" button for layer
                self.add_layer_effect_btn = tree.add_button(
                    -1, rect.x + 4.0, cy, content_w - 8.0, ADD_EFFECT_BTN_H,
                    UIStyle {
                        bg_color: ADD_EFFECT_BTN_BG,
                        hover_bg_color: ADD_EFFECT_BTN_HOVER,
                        text_color: ADD_EFFECT_BTN_TEXT,
                        corner_radius: 4.0,
                        text_align: TextAlign::Center,
                        font_size: 11,
                        ..UIStyle::default()
                    },
                    "+ Add Effect",
                ) as i32;
                cy += ADD_EFFECT_BTN_H + SECTION_GAP;
            }
            cy += SECTION_GAP;
        }

        // Clip section
        if self.clip_visible {
            let chrome_h = self.clip_chrome.compute_height();
            self.clip_chrome.build(tree, Rect::new(rect.x, cy, content_w, chrome_h));
            cy += chrome_h;

            if !self.clip_chrome.is_collapsed() {
                if let Some(ref mut gp) = self.gen_params {
                    let gp_h = gp.compute_height();
                    gp.build(tree, Rect::new(rect.x, cy, content_w, gp_h));
                    cy += gp_h + SECTION_GAP;
                }

                for card in &mut self.clip_effects {
                    let card_h = card.compute_height();
                    card.build(tree, Rect::new(rect.x, cy, content_w, card_h));
                    cy += card_h + SECTION_GAP;
                }
                // "+ Add Effect" button for clip
                self.add_clip_effect_btn = tree.add_button(
                    -1, rect.x + 4.0, cy, content_w - 8.0, ADD_EFFECT_BTN_H,
                    UIStyle {
                        bg_color: ADD_EFFECT_BTN_BG,
                        hover_bg_color: ADD_EFFECT_BTN_HOVER,
                        text_color: ADD_EFFECT_BTN_TEXT,
                        corner_radius: 4.0,
                        text_align: TextAlign::Center,
                        font_size: 11,
                        ..UIStyle::default()
                    },
                    "+ Add Effect",
                ) as i32;
                // cy not read after this point — total content height used by scroll logic
            }
        }

        // Reparent all content nodes (chrome + cards + buttons) under the clip region.
        // This ensures scrolled content is clipped to the inspector viewport and
        // doesn't bleed over the transport bar when scroll_offset > 0.
        let content_count = tree.count() - content_start;
        tree.reparent_root_nodes(content_start, content_count, clip_id);

        // Scrollbar track + thumb (NOT clipped — always visible at viewport edge)
        let sb_x = rect.x + content_w;
        self.scrollbar_track_id = tree.add_button(
            -1, sb_x, rect.y, SCROLLBAR_W, rect.height,
            UIStyle {
                bg_color: SCROLLBAR_TRACK_COLOR,
                hover_bg_color: Color32::new(
                    SCROLLBAR_TRACK_COLOR.r.saturating_add(10),
                    SCROLLBAR_TRACK_COLOR.g.saturating_add(10),
                    SCROLLBAR_TRACK_COLOR.b.saturating_add(10),
                    SCROLLBAR_TRACK_COLOR.a,
                ),
                ..UIStyle::default()
            },
            "",
        ) as i32;

        self.scrollbar_thumb_id = tree.add_button(
            -1, sb_x, rect.y, SCROLLBAR_W, SCROLLBAR_MIN_THUMB_H,
            UIStyle {
                bg_color: SCROLLBAR_THUMB_COLOR,
                hover_bg_color: SCROLLBAR_THUMB_HOVER,
                pressed_bg_color: Color32::new(
                    SCROLLBAR_THUMB_HOVER.r.saturating_sub(15),
                    SCROLLBAR_THUMB_HOVER.g.saturating_sub(15),
                    SCROLLBAR_THUMB_HOVER.b.saturating_sub(15),
                    SCROLLBAR_THUMB_HOVER.a,
                ),
                corner_radius: 2.0,
                ..UIStyle::default()
            },
            "",
        ) as i32;

        self.update_scroll_bounds();
        self.update_scrollbar(tree);
    }

    fn update(&mut self, _tree: &mut UITree) {
        // State sync is done via direct accessors on sub-panels.
        // The app layer calls sync methods like:
        //   inspector.master_chrome_mut().sync_opacity(&mut tree, 0.5);
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        match event {
            UIEvent::Click { node_id, pos, modifiers } => {
                if !self.viewport_rect.contains(*pos) { return Vec::new(); }
                self.route_click(*node_id, *modifiers)
            }
            UIEvent::PointerDown { node_id, pos } => {
                if !self.viewport_rect.contains(*pos) { return Vec::new(); }
                self.route_pointer_down(*node_id, *pos)
            }
            UIEvent::DragBegin { node_id, pos, .. } => {
                if !self.viewport_rect.contains(*pos) { return Vec::new(); }
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
            UIEvent::RightClick { node_id, pos, .. } => {
                if !self.viewport_rect.contains(*pos) { return Vec::new(); }
                self.route_right_click(*node_id)
            }
            _ => Vec::new(),
        }
    }
}

impl Default for InspectorCompositePanel {
    fn default() -> Self { Self::new() }
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
        assert!(panel.scrollbar_track_id >= 0);
        assert!(panel.scrollbar_thumb_id >= 0);
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
        panel.max_scroll = 100.0;
        panel.scroll_offset = 50.0;

        panel.handle_scroll(-100.0); // scroll way down
        assert!(panel.scroll_offset <= 100.0);

        panel.handle_scroll(100.0); // scroll way up
        assert!(panel.scroll_offset >= 0.0);
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
    fn content_height_changes_with_visibility() {
        let mut panel = InspectorCompositePanel::new();

        let full_h = panel.compute_content_height();
        panel.set_section_visible(InspectorTab::Master, false);
        let partial_h = panel.compute_content_height();

        assert!(partial_h < full_h);
    }

    #[test]
    fn find_target_for_scrollbar() {
        let mut tree = UITree::new();
        let mut panel = InspectorCompositePanel::new();
        let layout = inspector_layout();
        panel.build(&mut tree, &layout);

        let target = panel.find_target_for_node(panel.scrollbar_track_id as u32);
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

        let vp = panel.viewport_rect;
        let pos = Vec2::new(vp.x + 10.0, vp.y + 10.0);

        // Find the master chrome's chevron button and simulate click
        // We can test via route_click
        let actions = panel.route_click(panel.master_chrome.first_node() as u32 + 1, Modifiers::NONE);
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
        let sb_id = panel.scrollbar_thumb_id as u32;
        let pos = Vec2::new(280.0, 100.0);
        panel.route_pointer_down(sb_id, pos);

        assert!(panel.is_dragging());
        assert!(panel.dragging_scrollbar);
    }
}
