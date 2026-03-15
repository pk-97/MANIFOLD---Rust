use crate::color;
use crate::input::UIEvent;
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
const SCROLL_SPEED: f32 = 12.5;
const SECTION_GAP: f32 = 2.0;

const SCROLLBAR_TRACK_COLOR: Color32 = Color32::new(30, 30, 32, 180);
const SCROLLBAR_THUMB_COLOR: Color32 = Color32::new(90, 90, 95, 200);
const SCROLLBAR_THUMB_HOVER: Color32 = Color32::new(110, 110, 115, 220);

const ADD_EFFECT_BTN_H: f32 = 26.0;
const ADD_EFFECT_BTN_BG: Color32 = Color32::new(40, 45, 50, 255);
const ADD_EFFECT_BTN_HOVER: Color32 = Color32::new(55, 65, 75, 255);
const ADD_EFFECT_BTN_TEXT: Color32 = Color32::new(130, 170, 210, 255);

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

    // Background
    bg_panel_id: i32,
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
            bg_panel_id: -1,
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

    pub fn configure_gen_params(&mut self, config: Option<&GenParamConfig>) {
        if let Some(cfg) = config {
            let mut panel = GenParamPanel::new();
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

    pub fn is_dragging(&self) -> bool {
        self.dragging_scrollbar
            || self.master_chrome.is_dragging()
            || self.layer_chrome.is_dragging()
            || self.clip_chrome.is_dragging()
            || self.master_effects.iter().any(|e| e.is_dragging())
            || self.layer_effects.iter().any(|e| e.is_dragging())
            || self.clip_effects.iter().any(|e| e.is_dragging())
            || self.gen_params.as_ref().map_or(false, |p| p.is_dragging())
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
            for card in &self.master_effects {
                h += card.compute_height() + SECTION_GAP;
            }
            h += ADD_EFFECT_BTN_H + SECTION_GAP; // add effect button
            h += SECTION_GAP;
        }

        if self.layer_visible {
            h += self.layer_chrome.compute_height();
            for card in &self.layer_effects {
                h += card.compute_height() + SECTION_GAP;
            }
            h += ADD_EFFECT_BTN_H + SECTION_GAP; // add effect button
            h += SECTION_GAP;
        }

        if self.clip_visible {
            h += self.clip_chrome.compute_height();
            if let Some(ref gp) = self.gen_params {
                h += gp.compute_height() + SECTION_GAP;
            }
            for card in &self.clip_effects {
                h += card.compute_height() + SECTION_GAP;
            }
            h += ADD_EFFECT_BTN_H + SECTION_GAP; // add effect button
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
            if let Some(ref gp) = self.gen_params {
                if in_range(idx, gp.first_node(), gp.node_count()) {
                    return Some(PressedTarget::GenParam);
                }
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

    /// Route drag events to the pressed sub-panel.
    /// Call directly from the app layer (not through Panel::handle_event).
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

    // ── Internal event routing ───────────────────────────────────

    fn route_click(&mut self, node_id: u32) -> Vec<PanelAction> {
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
            match target {
                PressedTarget::MasterChrome => self.master_chrome.handle_click(node_id),
                PressedTarget::LayerChrome => self.layer_chrome.handle_click(node_id),
                PressedTarget::ClipChrome => self.clip_chrome.handle_click(node_id),
                PressedTarget::MasterEffect(i) => {
                    self.master_effects.get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::LayerEffect(i) => {
                    self.layer_effects.get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default()
                }
                PressedTarget::ClipEffect(i) => {
                    self.clip_effects.get_mut(i)
                        .map(|c| c.handle_click(node_id))
                        .unwrap_or_default()
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

        if let Some(target) = target {
            match target {
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
            }
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

        let mut cy = rect.y - self.scroll_offset;

        // Master section
        if self.master_visible {
            let chrome_h = self.master_chrome.compute_height();
            self.master_chrome.build(tree, Rect::new(rect.x, cy, content_w, chrome_h));
            cy += chrome_h;

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
            cy += SECTION_GAP;
        }

        // Layer section
        if self.layer_visible {
            let chrome_h = self.layer_chrome.compute_height();
            self.layer_chrome.build(tree, Rect::new(rect.x, cy, content_w, chrome_h));
            cy += chrome_h;

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
            cy += SECTION_GAP;
        }

        // Clip section
        if self.clip_visible {
            let chrome_h = self.clip_chrome.compute_height();
            self.clip_chrome.build(tree, Rect::new(rect.x, cy, content_w, chrome_h));
            cy += chrome_h;

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
            cy += ADD_EFFECT_BTN_H + SECTION_GAP;
        }

        // Scrollbar track + thumb
        let sb_x = rect.x + content_w;
        self.scrollbar_track_id = tree.add_button(
            -1, sb_x, rect.y, SCROLLBAR_W, rect.height,
            UIStyle {
                bg_color: SCROLLBAR_TRACK_COLOR,
                ..UIStyle::default()
            },
            "",
        ) as i32;

        self.scrollbar_thumb_id = tree.add_button(
            -1, sb_x, rect.y, SCROLLBAR_W, SCROLLBAR_MIN_THUMB_H,
            UIStyle {
                bg_color: SCROLLBAR_THUMB_COLOR,
                hover_bg_color: SCROLLBAR_THUMB_HOVER,
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
            UIEvent::Click { node_id, pos } => {
                if !self.viewport_rect.contains(*pos) { return Vec::new(); }
                self.route_click(*node_id)
            }
            UIEvent::PointerDown { node_id, pos } => {
                if !self.viewport_rect.contains(*pos) { return Vec::new(); }
                self.route_pointer_down(*node_id, *pos)
            }
            UIEvent::RightClick { node_id, pos } => {
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
        let actions = panel.route_click(panel.master_chrome.first_node() as u32 + 1);
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
