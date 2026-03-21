//! Grid-based browser popup for effect/generator selection.
//! Port of Unity BrowserPopupPanel.cs (632 lines).
//!
//! A floating modal with search bar, category chips, scrollable grid,
//! and optional paste button. Completely separate from DropdownPanel —
//! different layout, interaction, and rendering model.

use crate::color;
use crate::node::Color32;
use crate::node::*;
use crate::tree::UITree;
use super::{InspectorTab, PanelAction};

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
const CELL_FONT: u16 = 11;
const SEARCH_FONT: u16 = 11;
const SCROLL_SPEED: f32 = 12.5;
const ACCENT_BAR_W: f32 = 3.0;

// ── Colors ──

const BG_BORDER: Color32 = Color32::new(48, 48, 52, 255);
const BG_INNER: Color32 = Color32::new(19, 19, 20, 250);
const SEARCH_BG: Color32 = Color32::new(31, 31, 32, 255);
const SEARCH_TEXT: Color32 = Color32::new(168, 168, 172, 255);
const CELL_NORMAL: Color32 = Color32::new(36, 36, 38, 255);
const CELL_HOVER: Color32 = Color32::new(51, 51, 56, 255);
const CELL_SELECTED: Color32 = Color32::new(45, 65, 95, 255);
const CHIP_INACTIVE: Color32 = Color32::new(41, 41, 43, 255);
const PASTE_BG: Color32 = Color32::new(40, 40, 42, 255);
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
}

/// Result of an interaction.
#[derive(Debug, Clone)]
pub enum BrowserPopupAction {
    Selected(i32),
    Paste,
    Dismissed,
}

/// Request to open the popup.
pub struct BrowserPopupRequest {
    pub mode: BrowserPopupMode,
    pub tab: InspectorTab,
    pub item_names: Vec<String>,
    pub item_keys: Vec<i32>,
    pub item_categories: Vec<String>,
    pub category_names: Vec<String>,
    pub paste_count: usize,
    pub screen_anchor: Vec2,
}

// ── Panel ──

pub struct BrowserPopupPanel {
    is_open: bool,
    mode: BrowserPopupMode,
    tab: InspectorTab,

    // Source data
    item_names: Vec<String>,
    item_keys: Vec<i32>,
    item_categories: Vec<String>,
    category_names: Vec<String>,
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
    scroll_offset: f32,

    // Node IDs
    backdrop_id: i32,
    search_bar_id: i32,
    chip_all_id: i32,
    chip_ids: Vec<i32>,
    cell_ids: Vec<(i32, usize)>,  // (node_id, source_index)
    paste_id: i32,
    first_node: usize,
    node_count: usize,

    // Screen dimensions for edge clamping
    screen_w: f32,
    screen_h: f32,
}

impl BrowserPopupPanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            mode: BrowserPopupMode::Effect,
            tab: InspectorTab::Master,
            item_names: Vec::new(),
            item_keys: Vec::new(),
            item_categories: Vec::new(),
            category_names: Vec::new(),
            paste_count: 0,
            active_category: None,
            current_filter: String::new(),
            filtered_indices: Vec::new(),
            columns: 3,
            grid_viewport_height: 200.0,
            total_height: 300.0,
            popup_x: 100.0,
            popup_y: 100.0,
            scroll_offset: 0.0,
            backdrop_id: -1,
            search_bar_id: -1,
            chip_all_id: -1,
            chip_ids: Vec::new(),
            cell_ids: Vec::new(),
            paste_id: -1,
            first_node: 0,
            node_count: 0,
            screen_w: 1920.0,
            screen_h: 1080.0,
        }
    }

    pub fn is_open(&self) -> bool { self.is_open }
    pub fn first_node(&self) -> usize { self.first_node }
    pub fn tab(&self) -> InspectorTab { self.tab }

    pub fn set_screen_size(&mut self, w: f32, h: f32) {
        self.screen_w = w;
        self.screen_h = h;
    }

    pub fn open(&mut self, req: BrowserPopupRequest) {
        self.is_open = true;
        self.mode = req.mode;
        self.tab = req.tab;
        self.item_names = req.item_names;
        self.item_keys = req.item_keys;
        self.item_categories = req.item_categories;
        self.category_names = req.category_names;
        self.paste_count = req.paste_count;
        self.active_category = None;
        self.current_filter.clear();
        self.scroll_offset = 0.0;
        self.rebuild_filtered_list();
        self.compute_layout(req.screen_anchor);
    }

    pub fn close(&mut self) {
        self.is_open = false;
        self.item_names.clear();
        self.item_keys.clear();
        self.item_categories.clear();
        self.category_names.clear();
        self.filtered_indices.clear();
        self.cell_ids.clear();
        self.chip_ids.clear();
    }

    /// Called when the search filter changes (from TextInputManager commit).
    pub fn set_filter(&mut self, filter: String) {
        self.current_filter = filter;
        self.scroll_offset = 0.0;
        self.rebuild_filtered_list();
        // Recompute layout height with new count
        self.recompute_height();
    }

    pub fn set_category(&mut self, category: Option<String>) {
        self.active_category = category;
        self.scroll_offset = 0.0;
        self.rebuild_filtered_list();
        self.recompute_height();
    }

    // ── Filtering ──

    fn rebuild_filtered_list(&mut self) {
        self.filtered_indices.clear();
        let filter_lower = self.current_filter.to_lowercase();
        for (i, name) in self.item_names.iter().enumerate() {
            // Category filter
            if let Some(ref cat) = self.active_category {
                if i < self.item_categories.len() && self.item_categories[i] != *cat {
                    continue;
                }
            }
            // Search filter — case-insensitive substring
            if !filter_lower.is_empty() && !name.to_lowercase().contains(&filter_lower) {
                continue;
            }
            self.filtered_indices.push(i);
        }
    }

    // ── Layout ──

    fn compute_layout(&mut self, anchor: Vec2) {
        let inner_w = POPUP_WIDTH - PADDING * 2.0 - BORDER * 2.0;
        self.columns = ((inner_w + CELL_SPACING) / (CELL_WIDTH + CELL_SPACING)).floor().max(1.0) as usize;
        self.recompute_height();

        // Position: anchor the popup at the click position, edge-clamp
        self.popup_x = anchor.x;
        self.popup_y = anchor.y;

        // Right edge
        if self.popup_x + POPUP_WIDTH > self.screen_w {
            self.popup_x = self.screen_w - POPUP_WIDTH;
        }
        if self.popup_x < 0.0 { self.popup_x = 0.0; }

        // Bottom edge
        if self.popup_y + self.total_height > self.screen_h {
            self.popup_y = self.screen_h - self.total_height;
        }
        if self.popup_y < 0.0 { self.popup_y = 0.0; }
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
        let available = POPUP_MAX_HEIGHT - h - PADDING - BORDER
            - if has_paste { PASTE_BUTTON_HEIGHT + SECTION_SPACING } else { 0.0 };
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
        if !self.is_open { return; }

        self.first_node = tree.count();
        self.cell_ids.clear();
        self.chip_ids.clear();

        let px = self.popup_x;
        let py = self.popup_y;
        let pw = POPUP_WIDTH;
        let ph = self.total_height;

        // Fullscreen backdrop (dismiss on click outside)
        self.backdrop_id = tree.add_button(
            -1, 0.0, 0.0, self.screen_w, self.screen_h,
            UIStyle { bg_color: Color32::new(0, 0, 0, 80), ..UIStyle::default() },
            "",
        ) as i32;

        // Outer border
        tree.add_panel(-1, px, py, pw, ph,
            UIStyle { bg_color: BG_BORDER, corner_radius: 8.0, ..UIStyle::default() },
        );

        // Inner background
        tree.add_panel(-1, px + BORDER, py + BORDER, pw - BORDER * 2.0, ph - BORDER * 2.0,
            UIStyle { bg_color: BG_INNER, corner_radius: 7.0, ..UIStyle::default() },
        );

        let cx = px + BORDER + PADDING;
        let content_w = pw - BORDER * 2.0 - PADDING * 2.0;
        let mut cy = py + BORDER + PADDING;

        // Search bar
        self.search_bar_id = tree.add_button(
            -1, cx, cy, content_w, SEARCH_BAR_HEIGHT,
            UIStyle { bg_color: SEARCH_BG, corner_radius: 4.0, font_size: SEARCH_FONT, text_color: SEARCH_TEXT, ..UIStyle::default() },
            &if self.current_filter.is_empty() { "  Search...".to_string() } else { format!("  {}", self.current_filter) },
        ) as i32;
        cy += SEARCH_BAR_HEIGHT + SECTION_SPACING;

        // Category chips
        if !self.category_names.is_empty() {
            let mut chip_x = cx;
            let chip_h = CHIP_ROW_HEIGHT;

            // "All" chip
            let all_active = self.active_category.is_none();
            let all_w = estimate_chip_width("All");
            self.chip_all_id = tree.add_button(
                -1, chip_x, cy, all_w, chip_h,
                UIStyle {
                    bg_color: if all_active { color::ACCENT_BLUE } else { CHIP_INACTIVE },
                    corner_radius: chip_h * 0.5,
                    font_size: CELL_FONT,
                    text_color: if all_active { Color32::WHITE } else { TEXT_DIM },
                    ..UIStyle::default()
                },
                "All",
            ) as i32;
            chip_x += all_w + CHIP_SPACING;

            // Category chips
            for cat in &self.category_names {
                if cat == "Generators" { continue; } // Don't show "Generators" in effect browser
                let is_active = self.active_category.as_deref() == Some(cat.as_str());
                let w = estimate_chip_width(cat);
                let id = tree.add_button(
                    -1, chip_x, cy, w, chip_h,
                    UIStyle {
                        bg_color: if is_active { color::ACCENT_BLUE } else { CHIP_INACTIVE },
                        corner_radius: chip_h * 0.5,
                        font_size: CELL_FONT,
                        text_color: if is_active { Color32::WHITE } else { TEXT_DIM },
                        ..UIStyle::default()
                    },
                    &format!(" {} ", cat),
                ) as i32;
                self.chip_ids.push(id);
                chip_x += w + CHIP_SPACING;
            }
            cy += CHIP_ROW_HEIGHT + SECTION_SPACING;
        }

        // Grid viewport — ClipRegion clips cells that extend beyond bounds.
        let vp_top = cy;
        let vp_h = self.grid_viewport_height;

        let clip_id = tree.add_node(
            -1,
            Rect::new(cx, vp_top, content_w, vp_h),
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        ) as i32;

        for (fi, &src_idx) in self.filtered_indices.iter().enumerate() {
            let col = fi % self.columns;
            let row = fi / self.columns;
            // Relative Y for culling check (viewport-local)
            let rel_y = row as f32 * (CELL_HEIGHT + CELL_SPACING) - self.scroll_offset;

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
                tree.add_panel(clip_id, cell_x, cell_y, ACCENT_BAR_W, CELL_HEIGHT,
                    UIStyle { bg_color: accent_color, corner_radius: 2.0, ..UIStyle::default() },
                );
            }

            // Cell button — full height, ClipRegion handles visual clipping
            let prefix = if !self.item_categories.is_empty() { "     " } else { "  " };
            let label = format!("{}{}", prefix, &self.item_names[src_idx]);
            let id = tree.add_button(
                clip_id, cell_x, cell_y, CELL_WIDTH, CELL_HEIGHT,
                UIStyle {
                    bg_color: CELL_NORMAL,
                    corner_radius: CELL_RADIUS,
                    font_size: CELL_FONT,
                    text_color: TEXT_PRIMARY,
                    ..UIStyle::default()
                },
                &label,
            ) as i32;
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
            self.paste_id = tree.add_button(
                -1, cx, cy, content_w, PASTE_BUTTON_HEIGHT,
                UIStyle {
                    bg_color: PASTE_BG,
                    corner_radius: 4.0,
                    font_size: CELL_FONT,
                    text_color: color::ACCENT_BLUE,
                    ..UIStyle::default()
                },
                &paste_label,
            ) as i32;
        } else {
            self.paste_id = -1;
        }

        self.node_count = tree.count() - self.first_node;
    }

    // ── Event handling ──

    pub fn handle_click(&mut self, node_id: u32) -> Option<BrowserPopupAction> {
        if !self.is_open { return None; }

        let id = node_id as i32;

        // Backdrop → dismiss
        if id == self.backdrop_id {
            self.close();
            return Some(BrowserPopupAction::Dismissed);
        }

        // Search bar → signal to open text input
        if id == self.search_bar_id {
            return None; // Caller checks search_bar_clicked()
        }

        // "All" chip
        if id == self.chip_all_id {
            self.set_category(None);
            return None; // Needs rebuild, no action
        }

        // Category chips
        let cat_names: Vec<String> = self.category_names.iter()
            .filter(|c| c.as_str() != "Generators")
            .cloned()
            .collect();
        for (i, &chip_id) in self.chip_ids.iter().enumerate() {
            if id == chip_id && i < cat_names.len() {
                self.set_category(Some(cat_names[i].clone()));
                return None; // Needs rebuild
            }
        }

        // Grid cells
        for &(cell_id, src_idx) in &self.cell_ids {
            if id == cell_id {
                let key = self.item_keys[src_idx];
                self.close();
                return Some(BrowserPopupAction::Selected(key));
            }
        }

        // Paste button
        if id == self.paste_id && self.paste_id >= 0 {
            self.close();
            return Some(BrowserPopupAction::Paste);
        }

        None
    }

    /// Returns true if the search bar was the clicked node.
    pub fn is_search_bar(&self, node_id: u32) -> bool {
        self.search_bar_id >= 0 && node_id as i32 == self.search_bar_id
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
        if !self.is_open { return; }
        let rows = (self.filtered_indices.len() + self.columns - 1) / self.columns.max(1);
        let content_h = rows as f32 * (CELL_HEIGHT + CELL_SPACING) - CELL_SPACING;
        let max_scroll = (content_h - self.grid_viewport_height).max(0.0);
        self.scroll_offset = (self.scroll_offset - delta * SCROLL_SPEED).clamp(0.0, max_scroll);
    }

    /// Check if a node belongs to this popup.
    pub fn contains_node(&self, node_id: u32) -> bool {
        let id = node_id as usize;
        id >= self.first_node && id < self.first_node + self.node_count
    }

    /// Get search bar rect for text input anchoring.
    pub fn search_bar_rect(&self, tree: &UITree) -> Rect {
        if self.search_bar_id >= 0 {
            tree.get_bounds(self.search_bar_id as u32)
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
