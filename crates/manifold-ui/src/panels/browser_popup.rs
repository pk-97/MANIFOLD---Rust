//! Grid-based browser popup for effect/generator selection.
//! Port of Unity BrowserPopupPanel.cs (632 lines).
//!
//! A floating modal with search bar, category chips, scrollable grid,
//! and optional paste button. Completely separate from DropdownPanel —
//! different layout, interaction, and rendering model.

use super::InspectorTab;
use super::PanelAction;
use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse};
use super::popup_shell;
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::Color32;
use crate::node::*;
use crate::scroll_container::ScrollContainer;
use crate::tree::UITree;
use manifold_foundation::LayerId;

// ── Layout constants (from Unity BrowserPopupPanel.cs + BrowserPopupLayout.cs) ──

const POPUP_WIDTH: f32 = 600.0;
const POPUP_MAX_HEIGHT: f32 = 550.0;
const PADDING: f32 = 12.5;
const BORDER: f32 = 1.0;
const CELL_WIDTH: f32 = 185.0;
const CELL_HEIGHT: f32 = 42.5;
const CELL_SPACING: f32 = 3.75;
const SEARCH_BAR_HEIGHT: f32 = 35.0;
const CHIP_ROW_HEIGHT: f32 = 25.0;
const SECTION_SPACING: f32 = CELL_SPACING;
const PASTE_BUTTON_HEIGHT: f32 = 28.0;
const CELL_RADIUS: f32 = 6.0;
const CHIP_PAD_H: f32 = 10.0;
const CHIP_SPACING: f32 = 5.0;
const CHIP_FONT: f32 = 12.5;
const CELL_FONT: u16 = color::FONT_LABEL;
const SEARCH_FONT: u16 = color::FONT_LABEL;
const ACCENT_BAR_W: f32 = 3.0;

// ── Colors ──

const SEARCH_BG: Color32 = Color32::new(31, 31, 32, 255);
const SEARCH_TEXT: Color32 = Color32::new(168, 168, 172, 255);
const CELL_NORMAL: Color32 = Color32::new(36, 36, 38, 255);
const CELL_HOVER: Color32 = Color32::new(51, 51, 56, 255);
const CELL_PRESSED: Color32 = Color32::new(46, 46, 48, 255);
const CHIP_INACTIVE: Color32 = Color32::new(41, 41, 43, 255);
const CHIP_HOVER: Color32 = Color32::new(56, 56, 58, 255);
const PASTE_BG: Color32 = Color32::new(40, 40, 42, 255);
const PASTE_HOVER: Color32 = Color32::new(55, 55, 59, 255);
const SEARCH_HOVER: Color32 = Color32::new(38, 38, 40, 255);
const TEXT_PRIMARY: Color32 = Color32::new(224, 224, 224, 255);
const TEXT_DIM: Color32 = Color32::new(120, 120, 124, 255);

const CAT_SPATIAL: Color32 = Color32::new(102, 191, 191, 255);
const CAT_POST_PROCESS: Color32 = Color32::new(140, 160, 220, 255);
const CAT_FILMIC: Color32 = Color32::new(200, 180, 120, 255);
const CAT_SURVEILLANCE: Color32 = Color32::new(180, 100, 100, 255);

// ── Public types ──

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserPopupMode {
    Effect,
    Generator,
    /// Picking a graph node to spawn in the node editor. Items carry node
    /// `type_id`s in `item_type_ids` (not the i32 `item_keys`) and selection
    /// returns `NodeSelected`.
    Node,
}

/// Result of an interaction.
#[derive(Debug, Clone)]
pub enum BrowserPopupAction {
    /// Selection carries the popup's context atomically — prevents temporal coupling
    /// where context could be read after close() clears it.
    Selected {
        /// The chosen preset's stable type id (effect or generator), resolved
        /// directly by the dispatch with no registry-index indirection — so
        /// presets outside the startup-static registry (project-embedded /
        /// forked) are selectable.
        type_id: String,
        mode: BrowserPopupMode,
        tab: InspectorTab,
        layer_id: Option<LayerId>,
    },
    Paste,
    Dismissed,
    /// A node `type_id` was chosen in Node mode, to spawn at `graph_pos` (the
    /// graph-space cursor position captured when the picker opened).
    NodeSelected {
        type_id: String,
        graph_pos: (f32, f32),
    },
}

/// Request to open the popup.
pub struct BrowserPopupRequest {
    pub mode: BrowserPopupMode,
    pub tab: InspectorTab,
    /// For Generator mode: the layer whose generator type is being changed.
    pub layer_id: Option<LayerId>,
    pub item_names: Vec<String>,
    pub item_categories: Vec<String>,
    pub category_names: Vec<String>,
    /// The stable `type_id` per item (parallel to `item_names`), for every
    /// mode. Effect / generator selection resolves the chosen preset by this
    /// id; Node mode spawns it. This is the single selection key.
    pub item_type_ids: Vec<String>,
    /// Optional per-item search text (e.g. label plus descriptor aliases) the
    /// filter matches against instead of `item_names`. `None` keeps the
    /// name-only filter (Effect/Generator).
    pub item_search: Option<Vec<String>>,
    /// Node mode: graph-space position to spawn the chosen node at.
    pub spawn_graph_pos: Option<(f32, f32)>,
    pub paste_count: usize,
    pub screen_anchor: Vec2,
}

// ── Panel ──

pub struct BrowserPopupPanel {
    is_open: bool,
    mode: BrowserPopupMode,
    tab: InspectorTab,
    layer_id: Option<LayerId>,

    // Source data
    item_names: Vec<String>,
    item_categories: Vec<String>,
    category_names: Vec<String>,
    /// Parallel `type_id` per item; selection returns these (all modes).
    item_type_ids: Vec<String>,
    /// Optional search text per item (label + aliases) the filter uses when
    /// present, so typing "gaussian" or "Blur TOP" finds node.blur.
    item_search: Option<Vec<String>>,
    /// Node mode: graph-space spawn position, stashed across the per-frame
    /// tree rebuild so it survives from open to selection.
    pending_spawn_graph_pos: Option<(f32, f32)>,
    paste_count: usize,

    // Filter state
    active_category: Option<String>,
    pub current_filter: String,
    filtered_indices: Vec<usize>,

    // Layout
    columns: usize,
    grid_viewport_height: f32,
    total_height: f32,
    popup_x: f32,
    popup_y: f32,

    // Scroll
    scroll: ScrollContainer,

    // Node IDs
    backdrop_id: Option<NodeId>,
    search_bar_id: Option<NodeId>,
    chip_all_id: Option<NodeId>,
    chip_ids: Vec<NodeId>,
    cell_ids: Vec<(NodeId, usize)>, // (node_id, source_index)
    paste_id: Option<NodeId>,
    first_node: usize,
    node_count: usize,

    // Screen dimensions for edge clamping
    screen_w: f32,
    screen_h: f32,
}

impl Default for BrowserPopupPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserPopupPanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            mode: BrowserPopupMode::Effect,
            tab: InspectorTab::Master,
            layer_id: None,
            item_names: Vec::new(),
            item_categories: Vec::new(),
            category_names: Vec::new(),
            item_type_ids: Vec::new(),
            item_search: None,
            pending_spawn_graph_pos: None,
            paste_count: 0,
            active_category: None,
            current_filter: String::new(),
            filtered_indices: Vec::new(),
            columns: 3,
            grid_viewport_height: 200.0,
            total_height: 300.0,
            popup_x: 100.0,
            popup_y: 100.0,
            scroll: ScrollContainer::new(),
            backdrop_id: None,
            search_bar_id: None,
            chip_all_id: None,
            chip_ids: Vec::new(),
            cell_ids: Vec::new(),
            paste_id: None,
            first_node: 0,
            node_count: 0,
            screen_w: 1920.0,
            screen_h: 1080.0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }
    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn mode(&self) -> BrowserPopupMode {
        self.mode
    }
    pub fn tab(&self) -> InspectorTab {
        self.tab
    }
    pub fn layer_id(&self) -> &Option<LayerId> {
        &self.layer_id
    }

    pub fn set_screen_size(&mut self, w: f32, h: f32) {
        self.screen_w = w;
        self.screen_h = h;
    }

    pub fn open(&mut self, req: BrowserPopupRequest) {
        self.is_open = true;
        self.mode = req.mode;
        self.tab = req.tab;
        self.layer_id = req.layer_id;
        self.item_names = req.item_names;
        self.item_categories = req.item_categories;
        self.category_names = req.category_names;
        self.item_type_ids = req.item_type_ids;
        self.item_search = req.item_search;
        self.pending_spawn_graph_pos = req.spawn_graph_pos;
        self.paste_count = req.paste_count;
        self.active_category = None;
        self.current_filter.clear();
        self.scroll.reset();
        self.rebuild_filtered_list();
        self.compute_layout(req.screen_anchor);
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.layer_id = None;
        self.item_names.clear();
        self.item_categories.clear();
        self.category_names.clear();
        self.item_type_ids.clear();
        self.item_search = None;
        self.pending_spawn_graph_pos = None;
        self.filtered_indices.clear();
        self.cell_ids.clear();
        self.chip_ids.clear();
    }

    /// Called when the search filter changes (from TextInputManager commit).
    pub fn set_filter(&mut self, filter: String) {
        self.current_filter = filter;
        self.scroll.reset();
        self.rebuild_filtered_list();
        // Recompute layout height with new count
        self.recompute_height();
    }

    pub fn set_category(&mut self, category: Option<String>) {
        self.active_category = category;
        self.scroll.reset();
        self.rebuild_filtered_list();
        self.recompute_height();
    }

    // ── Filtering ──

    fn rebuild_filtered_list(&mut self) {
        self.filtered_indices.clear();
        let filter_lower = self.current_filter.to_lowercase();
        for i in 0..self.item_names.len() {
            // Category filter
            if let Some(ref cat) = self.active_category
                && i < self.item_categories.len()
                && self.item_categories[i] != *cat
            {
                continue;
            }
            // Search filter — case-insensitive substring against the search
            // text (label + aliases) when provided, else the display name.
            if !filter_lower.is_empty() {
                let haystack = self
                    .item_search
                    .as_ref()
                    .and_then(|s| s.get(i))
                    .unwrap_or(&self.item_names[i]);
                if !haystack.to_lowercase().contains(&filter_lower) {
                    continue;
                }
            }
            self.filtered_indices.push(i);
        }
    }

    // ── Layout ──

    fn compute_layout(&mut self, anchor: Vec2) {
        let inner_w = POPUP_WIDTH - PADDING * 2.0 - BORDER * 2.0;
        self.columns = ((inner_w + CELL_SPACING) / (CELL_WIDTH + CELL_SPACING))
            .floor()
            .max(1.0) as usize;
        self.recompute_height();

        // Position: anchor the popup at the click position, edge-clamp
        self.popup_x = anchor.x;
        self.popup_y = anchor.y;

        // Right edge
        if self.popup_x + POPUP_WIDTH > self.screen_w {
            self.popup_x = self.screen_w - POPUP_WIDTH;
        }
        if self.popup_x < 0.0 {
            self.popup_x = 0.0;
        }

        // Bottom edge
        if self.popup_y + self.total_height > self.screen_h {
            self.popup_y = self.screen_h - self.total_height;
        }
        if self.popup_y < 0.0 {
            self.popup_y = 0.0;
        }
    }

    fn recompute_height(&mut self) {
        let has_chips = !self.category_names.is_empty();
        let has_paste = self.paste_count > 0;
        let rows = (self.filtered_indices.len() + self.columns - 1) / self.columns.max(1);
        let grid_content_h = rows as f32 * (CELL_HEIGHT + CELL_SPACING) - CELL_SPACING;

        let mut h = BORDER + PADDING;
        h += SEARCH_BAR_HEIGHT + SECTION_SPACING;
        if has_chips {
            h += CHIP_ROW_HEIGHT + SECTION_SPACING;
        }

        // Grid viewport — clamp to reasonable height
        let available = POPUP_MAX_HEIGHT
            - h
            - PADDING
            - BORDER
            - if has_paste {
                PASTE_BUTTON_HEIGHT + SECTION_SPACING
            } else {
                0.0
            };
        self.grid_viewport_height = grid_content_h.min(available).max(CELL_HEIGHT);

        h += self.grid_viewport_height;
        if has_paste {
            h += SECTION_SPACING + PASTE_BUTTON_HEIGHT;
        }
        h += PADDING + BORDER;
        self.total_height = h;
    }

    // ── Build ──

    pub fn build(&mut self, tree: &mut UITree) {
        if !self.is_open {
            return;
        }

        self.first_node = tree.count();
        self.cell_ids.clear();
        self.chip_ids.clear();

        let px = self.popup_x;
        let py = self.popup_y;
        let pw = POPUP_WIDTH;
        let ph = self.total_height;

        // Scrim + modal container via the shared shell (§17 lifts it with a
        // soft shadow; search bar / chips / grid are added on top as siblings).
        let shell = popup_shell::build(
            tree,
            (self.screen_w, self.screen_h),
            Rect::new(px, py, pw, ph),
            &popup_shell::PopupStyle::MODAL,
        );
        self.backdrop_id = Some(shell.backdrop);

        let cx = px + BORDER + PADDING;
        let content_w = pw - BORDER * 2.0 - PADDING * 2.0;
        let mut cy = py + BORDER + PADDING;

        // Search bar
        self.search_bar_id = Some(tree.add_button(
            None,
            cx,
            cy,
            content_w,
            SEARCH_BAR_HEIGHT,
            UIStyle {
                bg_color: SEARCH_BG,
                hover_bg_color: SEARCH_HOVER,
                corner_radius: color::BUTTON_RADIUS,
                font_size: SEARCH_FONT,
                text_color: SEARCH_TEXT,
                ..UIStyle::default()
            },
            &if self.current_filter.is_empty() {
                "  Search...".to_string()
            } else {
                format!("  {}", self.current_filter)
            },
        ));
        cy += SEARCH_BAR_HEIGHT + SECTION_SPACING;

        // Category chips
        if !self.category_names.is_empty() {
            let mut chip_x = cx;
            let chip_h = CHIP_ROW_HEIGHT;

            // "All" chip
            let all_active = self.active_category.is_none();
            let all_w = estimate_chip_width("All");
            self.chip_all_id = Some(tree.add_button(
                None,
                chip_x,
                cy,
                all_w,
                chip_h,
                UIStyle {
                    bg_color: if all_active {
                        color::ACCENT_BLUE
                    } else {
                        CHIP_INACTIVE
                    },
                    hover_bg_color: if all_active {
                        color::ACCENT_BLUE
                    } else {
                        CHIP_HOVER
                    },
                    corner_radius: chip_h * 0.5,
                    font_size: CELL_FONT,
                    text_color: if all_active { Color32::WHITE } else { TEXT_DIM },
                    ..UIStyle::default()
                },
                "All",
            ));
            chip_x += all_w + CHIP_SPACING;

            // Category chips
            for cat in &self.category_names {
                if cat == "Generators" {
                    continue;
                } // Don't show "Generators" in effect browser
                let is_active = self.active_category.as_deref() == Some(cat.as_str());
                let w = estimate_chip_width(cat);
                let id = tree.add_button(
                    None,
                    chip_x,
                    cy,
                    w,
                    chip_h,
                    UIStyle {
                        bg_color: if is_active {
                            color::ACCENT_BLUE
                        } else {
                            CHIP_INACTIVE
                        },
                        hover_bg_color: if is_active {
                            color::ACCENT_BLUE
                        } else {
                            CHIP_HOVER
                        },
                        corner_radius: chip_h * 0.5,
                        font_size: CELL_FONT,
                        text_color: if is_active { Color32::WHITE } else { TEXT_DIM },
                        ..UIStyle::default()
                    },
                    &format!(" {} ", cat),
                );
                self.chip_ids.push(id);
                chip_x += w + CHIP_SPACING;
            }
            cy += CHIP_ROW_HEIGHT + SECTION_SPACING;
        }

        // Grid viewport — ClipRegion clips cells that extend beyond bounds.
        let vp_top = cy;
        let vp_h = self.grid_viewport_height;

        let clip_parent = Some(self.scroll.begin(tree, Rect::new(cx, vp_top, content_w, vp_h)));

        for (fi, &src_idx) in self.filtered_indices.iter().enumerate() {
            let col = fi % self.columns;
            let row = fi / self.columns;
            // Relative Y for culling check (viewport-local)
            let rel_y = row as f32 * (CELL_HEIGHT + CELL_SPACING) - self.scroll.scroll_offset();

            // Cull cells entirely outside viewport
            if rel_y + CELL_HEIGHT < 0.0 || rel_y > vp_h {
                continue;
            }

            // Absolute positions — UITree uses screen coordinates for all nodes
            let cell_x = cx + col as f32 * (CELL_WIDTH + CELL_SPACING);
            let cell_y = vp_top + rel_y;

            // Category accent bar
            if src_idx < self.item_categories.len() && !self.item_categories[src_idx].is_empty() {
                let accent_color = category_color(&self.item_categories[src_idx]);
                tree.add_panel(
                    clip_parent,
                    cell_x,
                    cell_y,
                    ACCENT_BAR_W,
                    CELL_HEIGHT,
                    UIStyle {
                        bg_color: accent_color,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
            }

            // Cell button — full height, ClipRegion handles visual clipping
            let prefix = if !self.item_categories.is_empty() {
                "     "
            } else {
                "  "
            };
            let label = format!("{}{}", prefix, &self.item_names[src_idx]);
            let id = tree.add_button(
                clip_parent,
                cell_x,
                cell_y,
                CELL_WIDTH,
                CELL_HEIGHT,
                UIStyle {
                    bg_color: CELL_NORMAL,
                    hover_bg_color: CELL_HOVER,
                    pressed_bg_color: CELL_PRESSED,
                    corner_radius: CELL_RADIUS,
                    font_size: CELL_FONT,
                    text_color: TEXT_PRIMARY,
                    ..UIStyle::default()
                },
                &label,
            );
            self.cell_ids.push((id, src_idx));
        }

        cy += vp_h;

        // Paste button
        if self.paste_count > 0 {
            cy += SECTION_SPACING;
            let paste_label = if self.paste_count == 1 {
                "Paste Effect".to_string()
            } else {
                format!("Paste {} Effects", self.paste_count)
            };
            self.paste_id = Some(tree.add_button(
                None,
                cx,
                cy,
                content_w,
                PASTE_BUTTON_HEIGHT,
                UIStyle {
                    bg_color: PASTE_BG,
                    hover_bg_color: PASTE_HOVER,
                    corner_radius: color::BUTTON_RADIUS,
                    font_size: CELL_FONT,
                    text_color: color::ACCENT_BLUE,
                    ..UIStyle::default()
                },
                &paste_label,
            ));
        } else {
            self.paste_id = None;
        }

        self.node_count = tree.count() - self.first_node;
    }

    // ── Event handling ──

    pub fn handle_click(&mut self, node_id: NodeId) -> Option<BrowserPopupAction> {
        if !self.is_open {
            return None;
        }

        // Backdrop → dismiss
        if self.backdrop_id == Some(node_id) {
            self.close();
            return Some(BrowserPopupAction::Dismissed);
        }

        // Search bar → signal to open text input
        if self.search_bar_id == Some(node_id) {
            return None; // Caller checks search_bar_clicked()
        }

        // "All" chip
        if self.chip_all_id == Some(node_id) {
            self.set_category(None);
            return None; // Needs rebuild, no action
        }

        // Category chips
        let cat_names: Vec<String> = self
            .category_names
            .iter()
            .filter(|c| c.as_str() != "Generators")
            .cloned()
            .collect();
        for (i, &chip_id) in self.chip_ids.iter().enumerate() {
            if node_id == chip_id && i < cat_names.len() {
                self.set_category(Some(cat_names[i].clone()));
                return None; // Needs rebuild
            }
        }

        // Grid cells
        for &(cell_id, src_idx) in &self.cell_ids {
            if node_id == cell_id {
                // Capture context BEFORE close() clears it. Node mode returns
                // the type_id + stashed spawn position; the effect/generator
                // path is unchanged.
                let action = if self.mode == BrowserPopupMode::Node {
                    BrowserPopupAction::NodeSelected {
                        type_id: self.item_type_ids.get(src_idx).cloned().unwrap_or_default(),
                        graph_pos: self.pending_spawn_graph_pos.unwrap_or((0.0, 0.0)),
                    }
                } else {
                    BrowserPopupAction::Selected {
                        type_id: self.item_type_ids.get(src_idx).cloned().unwrap_or_default(),
                        mode: self.mode,
                        tab: self.tab,
                        layer_id: self.layer_id.clone(),
                    }
                };
                self.close();
                return Some(action);
            }
        }

        // Paste button
        if self.paste_id == Some(node_id) {
            self.close();
            return Some(BrowserPopupAction::Paste);
        }

        None
    }

    /// Returns true if the search bar was the clicked node.
    pub fn is_search_bar(&self, node_id: NodeId) -> bool {
        self.search_bar_id == Some(node_id)
    }

    /// Handle escape key.
    pub fn handle_escape(&mut self) -> Option<BrowserPopupAction> {
        if self.is_open {
            self.close();
            Some(BrowserPopupAction::Dismissed)
        } else {
            None
        }
    }

    /// Handle mouse wheel scroll within the popup.
    pub fn handle_scroll(&mut self, delta: f32) {
        if !self.is_open {
            return;
        }
        let rows = (self.filtered_indices.len() + self.columns - 1) / self.columns.max(1);
        let content_h = rows as f32 * (CELL_HEIGHT + CELL_SPACING) - CELL_SPACING;
        self.scroll.set_content_height(content_h);
        self.scroll.apply_scroll_delta(delta);
    }

    /// Check if a node belongs to this popup.
    pub fn contains_node(&self, node_id: NodeId) -> bool {
        let id = node_id.index();
        id >= self.first_node && id < self.first_node + self.node_count
    }

    /// Get search bar rect for text input anchoring.
    pub fn search_bar_rect(&self, tree: &UITree) -> Rect {
        if let Some(id) = self.search_bar_id {
            tree.get_bounds(id)
        } else {
            Rect::ZERO
        }
    }
}

// ── Helpers ──

fn estimate_chip_width(label: &str) -> f32 {
    label.len() as f32 * CHIP_FONT * 0.6 + CHIP_PAD_H * 2.0
}

fn category_color(category: &str) -> Color32 {
    match category {
        "Spatial" => CAT_SPATIAL,
        "Post-Process" => CAT_POST_PROCESS,
        "Filmic" => CAT_FILMIC,
        "Surveillance" => CAT_SURVEILLANCE,
        _ => TEXT_DIM,
    }
}

impl Overlay for BrowserPopupPanel {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn modality(&self) -> Modality {
        // The popup builds its own full-screen backdrop node, so the driver
        // must not add a second scrim.
        Modality::Modal {
            dim_background: false,
        }
    }

    fn anchor(&self) -> Anchor {
        // Click-anchored and content-sized; positions itself in build().
        Anchor::SelfManaged
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::ZERO
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        self.set_screen_size(placement.screen.x, placement.screen.y);
        self.build(tree);
    }

    fn on_event(&mut self, event: &UIEvent, _tree: &mut UITree) -> OverlayResponse {
        if !self.is_open() {
            return OverlayResponse::Ignored;
        }
        match event {
            UIEvent::KeyDown { key: Key::Escape, .. } => {
                self.handle_escape();
                OverlayResponse::Consumed(Vec::new())
            }
            UIEvent::Click { node_id, .. } => {
                if self.is_search_bar(*node_id) {
                    return OverlayResponse::Consumed(vec![PanelAction::BrowserSearchClicked]);
                }
                match self.handle_click(*node_id) {
                    Some(BrowserPopupAction::Selected {
                        type_id,
                        mode,
                        tab,
                        layer_id,
                    }) => {
                        let action = match mode {
                            BrowserPopupMode::Effect => PanelAction::AddEffect(
                                tab,
                                crate::types::PresetTypeId::from_string(type_id),
                            ),
                            BrowserPopupMode::Generator => PanelAction::SetGenType(
                                layer_id,
                                crate::types::PresetTypeId::from_string(type_id),
                            ),
                            // Node mode is editor-window only; never reached on
                            // the main-window overlay path.
                            BrowserPopupMode::Node => {
                                return OverlayResponse::Consumed(Vec::new());
                            }
                        };
                        OverlayResponse::Consumed(vec![action])
                    }
                    Some(BrowserPopupAction::Paste) => {
                        OverlayResponse::Consumed(vec![PanelAction::PasteEffects])
                    }
                    // Dismissed (incl. backdrop), or an internal chip/category
                    // click that needs a rebuild — consume so the modal swallows
                    // it and the driver re-runs build_at next tick.
                    _ => OverlayResponse::Consumed(Vec::new()),
                }
            }
            UIEvent::Scroll { delta, .. } => {
                self.handle_scroll(delta.y);
                OverlayResponse::Consumed(Vec::new())
            }
            _ => OverlayResponse::Ignored,
        }
    }
}
