//! Dropdown & context menu overlay.
//!
//! A floating panel that renders above all other UI. Used for:
//! - Dropdown menus (triggered by a button, positioned below it)
//! - Context menus (triggered by right-click, positioned at cursor)
//!
//! The app layer is responsible for:
//! 1. Calling `open()` / `open_context()` with items and position.
//! 2. Routing *all* UIEvents to `handle_event()` FIRST (before other panels).
//! 3. Acting on the returned `DropdownAction`.
//! 4. The dropdown auto-dismisses on selection or click-outside.

use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants ───────────────────────────────────────────────

const ITEM_HEIGHT: f32 = 24.0;
const PADDING_H: f32 = 8.0;
const PADDING_V: f32 = 4.0;
const MIN_WIDTH: f32 = 120.0;
const MAX_VISIBLE_ITEMS: usize = 20;
const SEPARATOR_HEIGHT: f32 = 9.0; // pad + 1px line + pad
const CHAR_WIDTH: f32 = 7.0; // approximate glyph width for width estimation

/// A single item in a dropdown menu.
#[derive(Debug, Clone)]
pub struct DropdownItem {
    pub label: String,
    pub enabled: bool,
    pub separator_after: bool,
    /// Optional checkmark or other indicator.
    pub checked: bool,
}

impl DropdownItem {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            enabled: true,
            separator_after: false,
            checked: false,
        }
    }

    pub fn disabled(label: &str) -> Self {
        Self {
            label: label.to_string(),
            enabled: false,
            separator_after: false,
            checked: false,
        }
    }

    pub fn with_separator(mut self) -> Self {
        self.separator_after = true;
        self
    }

    pub fn with_check(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
}

/// Actions returned by the dropdown panel.
#[derive(Debug, Clone, PartialEq)]
pub enum DropdownAction {
    /// User selected item at this index.
    Selected(usize),
    /// User dismissed the dropdown (clicked outside or pressed Escape).
    Dismissed,
}

/// Floating dropdown / context menu overlay.
pub struct DropdownPanel {
    is_open: bool,
    items: Vec<DropdownItem>,
    /// Screen-space anchor point (top-left of dropdown).
    anchor: Vec2,
    /// Screen dimensions for edge clamping.
    screen_width: f32,
    screen_height: f32,
    /// Minimum width (can be larger than MIN_WIDTH for wide content).
    min_width: f32,
    /// Computed bounds of the dropdown container.
    container_bounds: Rect,
    /// Node IDs.
    root_id: i32,
    item_ids: Vec<i32>,
    separator_ids: Vec<i32>,
    /// Index of currently hovered item (-1 = none).
    hovered_index: i32,
    /// First node index in the tree (for node range checks).
    first_node: usize,
    node_count: usize,
}

impl DropdownPanel {
    pub fn new() -> Self {
        Self {
            is_open: false,
            items: Vec::new(),
            anchor: Vec2::ZERO,
            screen_width: 1920.0,
            screen_height: 1080.0,
            min_width: MIN_WIDTH,
            container_bounds: Rect::ZERO,
            root_id: -1,
            item_ids: Vec::new(),
            separator_ids: Vec::new(),
            hovered_index: -1,
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }

    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// Set screen dimensions for edge clamping.
    pub fn set_screen_size(&mut self, width: f32, height: f32) {
        self.screen_width = width;
        self.screen_height = height;
    }

    /// Open as a dropdown below a trigger button.
    pub fn open(
        &mut self,
        items: Vec<DropdownItem>,
        trigger_rect: Rect,
        min_width: f32,
        tree: &mut UITree,
    ) {
        let anchor = Vec2::new(trigger_rect.x, trigger_rect.y_max());
        self.open_at(items, anchor, min_width.max(trigger_rect.width), tree);
    }

    /// Open as a context menu at the given screen position.
    pub fn open_context(
        &mut self,
        items: Vec<DropdownItem>,
        pos: Vec2,
        tree: &mut UITree,
    ) {
        self.open_at(items, pos, MIN_WIDTH, tree);
    }

    fn open_at(
        &mut self,
        items: Vec<DropdownItem>,
        anchor: Vec2,
        min_width: f32,
        tree: &mut UITree,
    ) {
        self.items = items;
        self.min_width = min_width;
        self.hovered_index = -1;
        self.is_open = true;

        // Compute content dimensions.
        let visible_count = self.items.len().min(MAX_VISIBLE_ITEMS);
        let content_width = self.compute_content_width();
        let w = content_width.max(self.min_width);

        let mut h = PADDING_V * 2.0;
        for (i, item) in self.items.iter().enumerate() {
            if i >= visible_count {
                break;
            }
            h += ITEM_HEIGHT;
            if item.separator_after {
                h += SEPARATOR_HEIGHT;
            }
        }

        // Edge clamping — clamp both position AND size to screen.
        let w = w.min(self.screen_width);
        let h = h.min(self.screen_height);
        let mut x = anchor.x;
        let mut y = anchor.y;
        if x + w > self.screen_width {
            x = (self.screen_width - w).max(0.0);
        }
        if y + h > self.screen_height {
            // Try placing above the anchor instead.
            let above_y = anchor.y - h;
            if above_y >= 0.0 {
                y = above_y;
            } else {
                y = (self.screen_height - h).max(0.0);
            }
        }

        self.anchor = Vec2::new(x, y);
        self.container_bounds = Rect::new(x, y, w, h);

        self.build_nodes(tree);
    }

    /// Close the dropdown and hide all nodes.
    pub fn close(&mut self, tree: &mut UITree) {
        if !self.is_open {
            return;
        }
        self.is_open = false;
        if self.root_id >= 0 {
            tree.set_visible(self.root_id as u32, false);
        }
    }

    fn build_nodes(&mut self, tree: &mut UITree) {
        self.first_node = tree.count();
        self.item_ids.clear();
        self.separator_ids.clear();

        let bounds = self.container_bounds;

        // Root container with border + shadow bg.
        let container_style = UIStyle {
            bg_color: color::DROPDOWN_BG,
            border_color: color::CARD_BORDER_C32,
            corner_radius: color::POPUP_RADIUS,
            border_width: 1.0,
            ..UIStyle::default()
        };
        self.root_id = tree.add_panel(-1, bounds.x, bounds.y, bounds.width, bounds.height, container_style) as i32;

        // Build items.
        let mut cy = PADDING_V;
        let item_w = bounds.width - PADDING_H * 2.0;
        let visible_count = self.items.len().min(MAX_VISIBLE_ITEMS);

        for i in 0..visible_count {
            let item = &self.items[i];
            let label = item.label.clone();
            let enabled = item.enabled;
            let checked = item.checked;
            let separator_after = item.separator_after;

            let text_color = if enabled {
                color::TEXT_NORMAL
            } else {
                color::TEXT_SUBTLE
            };

            let display_text = if checked {
                format!("\u{2713} {}", label)
            } else {
                label
            };

            let item_style = UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: if enabled { color::DROPDOWN_HIGHLIGHT } else { Color32::TRANSPARENT },
                pressed_bg_color: if enabled { color::DROPDOWN_PRESSED_BG } else { Color32::TRANSPARENT },
                text_color,
                font_size: color::FONT_BODY,
                text_align: TextAlign::Left,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            };

            let flags = if enabled {
                UIFlags::INTERACTIVE
            } else {
                UIFlags::DISABLED
            };

            let id = tree.add_node(
                self.root_id,
                Rect::new(PADDING_H, cy, item_w, ITEM_HEIGHT),
                UINodeType::Button,
                item_style,
                Some(&display_text),
                flags,
            );
            self.item_ids.push(id as i32);
            cy += ITEM_HEIGHT;

            if separator_after {
                let sep_y = cy + SEPARATOR_HEIGHT / 2.0 - 0.5;
                let sep_style = UIStyle {
                    bg_color: color::DIVIDER_C32,
                    ..UIStyle::default()
                };
                let sep_id = tree.add_panel(
                    self.root_id,
                    PADDING_H,
                    sep_y,
                    item_w,
                    1.0,
                    sep_style,
                );
                self.separator_ids.push(sep_id as i32);
                cy += SEPARATOR_HEIGHT;
            }
        }

        self.node_count = tree.count() - self.first_node;
    }

    /// Handle a UI event. Returns an action if the event was consumed.
    /// The app layer should call this BEFORE routing events to other panels.
    /// If it returns Some(...), the event was consumed and should not propagate.
    pub fn handle_event(&mut self, event: &UIEvent, tree: &mut UITree) -> Option<DropdownAction> {
        if !self.is_open {
            return None;
        }

        match event {
            UIEvent::Click { node_id, .. } => {
                // Check if clicked on one of our items.
                if let Some(index) = self.item_index_for_node(*node_id) {
                    if self.items[index].enabled {
                        self.close(tree);
                        return Some(DropdownAction::Selected(index));
                    }
                    // Clicked disabled item — consume but don't dismiss.
                    return Some(DropdownAction::Dismissed);
                }
                // Click outside → dismiss.
                self.close(tree);
                Some(DropdownAction::Dismissed)
            }
            UIEvent::HoverEnter { node_id, .. } => {
                if let Some(index) = self.item_index_for_node(*node_id) {
                    self.hovered_index = index as i32;
                }
                None
            }
            UIEvent::HoverExit { node_id } => {
                if let Some(index) = self.item_index_for_node(*node_id) {
                    if self.hovered_index == index as i32 {
                        self.hovered_index = -1;
                    }
                }
                None
            }
            UIEvent::KeyDown { key, .. } => {
                match key {
                    crate::input::Key::Escape => {
                        self.close(tree);
                        Some(DropdownAction::Dismissed)
                    }
                    crate::input::Key::Enter => {
                        if self.hovered_index >= 0 {
                            let idx = self.hovered_index as usize;
                            if idx < self.items.len() && self.items[idx].enabled {
                                self.close(tree);
                                return Some(DropdownAction::Selected(idx));
                            }
                        }
                        None
                    }
                    crate::input::Key::Down => {
                        self.move_hover(1);
                        None
                    }
                    crate::input::Key::Up => {
                        self.move_hover(-1);
                        None
                    }
                    _ => None,
                }
            }
            // Consume right-clicks and drags while open.
            UIEvent::RightClick { .. }
            | UIEvent::DragBegin { .. }
            | UIEvent::Drag { .. }
            | UIEvent::DragEnd { .. } => {
                self.close(tree);
                Some(DropdownAction::Dismissed)
            }
            _ => None,
        }
    }

    fn item_index_for_node(&self, node_id: u32) -> Option<usize> {
        let nid = node_id as i32;
        self.item_ids.iter().position(|&id| id == nid)
    }

    fn move_hover(&mut self, direction: i32) {
        if self.items.is_empty() {
            return;
        }
        let count = self.items.len().min(MAX_VISIBLE_ITEMS) as i32;
        let mut next = self.hovered_index + direction;

        // Wrap around.
        if next < 0 {
            next = count - 1;
        } else if next >= count {
            next = 0;
        }

        // Skip disabled items (max one full loop).
        let start = next;
        loop {
            if next >= 0 && (next as usize) < self.items.len() && self.items[next as usize].enabled {
                self.hovered_index = next;
                return;
            }
            next += direction;
            if next < 0 {
                next = count - 1;
            } else if next >= count {
                next = 0;
            }
            if next == start {
                break; // All disabled, bail.
            }
        }
    }

    fn compute_content_width(&self) -> f32 {
        let mut max_chars = 0usize;
        for item in &self.items {
            let len = item.label.len() + if item.checked { 2 } else { 0 };
            if len > max_chars {
                max_chars = len;
            }
        }
        (max_chars as f32 * CHAR_WIDTH + PADDING_H * 2.0 + 16.0).max(MIN_WIDTH)
    }
}

impl Default for DropdownPanel {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_items() -> Vec<DropdownItem> {
        vec![
            DropdownItem::new("Cut"),
            DropdownItem::new("Copy"),
            DropdownItem::new("Paste").with_separator(),
            DropdownItem::new("Select All"),
        ]
    }

    #[test]
    fn open_builds_nodes() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = make_items();
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        assert!(dd.is_open());
        assert_eq!(dd.item_ids.len(), 4);
        // 1 root + 4 items + 1 separator = 6 nodes
        assert_eq!(dd.node_count(), 6);
        assert!(tree.count() >= 6);
    }

    #[test]
    fn click_item_selects_and_closes() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = make_items();
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        let copy_id = dd.item_ids[1] as u32;
        let event = UIEvent::Click {
            node_id: copy_id,
            pos: Vec2::new(110.0, 230.0),
        };
        let result = dd.handle_event(&event, &mut tree);
        assert_eq!(result, Some(DropdownAction::Selected(1)));
        assert!(!dd.is_open());
    }

    #[test]
    fn click_outside_dismisses() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = make_items();
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        // Click on a node that isn't ours (simulate with a dummy node id).
        let event = UIEvent::Click {
            node_id: 999,
            pos: Vec2::new(500.0, 500.0),
        };
        let result = dd.handle_event(&event, &mut tree);
        assert_eq!(result, Some(DropdownAction::Dismissed));
        assert!(!dd.is_open());
    }

    #[test]
    fn escape_dismisses() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = make_items();
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        let event = UIEvent::KeyDown {
            node_id: 0,
            key: crate::input::Key::Escape,
            modifiers: crate::input::Modifiers { shift: false, ctrl: false, alt: false, command: false },
        };
        let result = dd.handle_event(&event, &mut tree);
        assert_eq!(result, Some(DropdownAction::Dismissed));
        assert!(!dd.is_open());
    }

    #[test]
    fn edge_clamping_right() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(200.0, 1080.0); // Narrow screen.

        let items = vec![DropdownItem::new("A very long menu item name")];
        dd.open_context(items, Vec2::new(180.0, 50.0), &mut tree);

        // Container should be clamped so it doesn't go off-screen.
        assert!(dd.container_bounds.x + dd.container_bounds.width <= 200.0);
    }

    #[test]
    fn edge_clamping_bottom() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 100.0); // Short screen.

        let items = make_items();
        dd.open_context(items, Vec2::new(50.0, 90.0), &mut tree);

        // Should flip above or clamp.
        assert!(dd.container_bounds.y + dd.container_bounds.height <= 100.0);
    }

    #[test]
    fn disabled_item_not_selectable() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = vec![
            DropdownItem::new("Enabled"),
            DropdownItem::disabled("Disabled"),
        ];
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        let disabled_id = dd.item_ids[1] as u32;
        let event = UIEvent::Click {
            node_id: disabled_id,
            pos: Vec2::new(110.0, 240.0),
        };
        let result = dd.handle_event(&event, &mut tree);
        // Clicking disabled item dismisses but doesn't select.
        assert_eq!(result, Some(DropdownAction::Dismissed));
    }

    #[test]
    fn keyboard_navigation() {
        let mut dd = DropdownPanel::new();
        dd.items = vec![
            DropdownItem::new("A"),
            DropdownItem::disabled("B"),
            DropdownItem::new("C"),
        ];
        dd.is_open = true;
        dd.hovered_index = -1;

        // Down arrow should skip to first enabled.
        dd.move_hover(1);
        assert_eq!(dd.hovered_index, 0);

        // Down again — skip disabled B, land on C.
        dd.move_hover(1);
        // From 0, next is 1 (disabled), skip to 2.
        // Actually move_hover starts from hovered_index + direction.
        // hovered=0, +1=1 (disabled), +1=2 (enabled) → 2.
        assert_eq!(dd.hovered_index, 2);

        // Down again — wrap to 0.
        dd.move_hover(1);
        assert_eq!(dd.hovered_index, 0);
    }

    #[test]
    fn open_dropdown_below_trigger() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let trigger = Rect::new(50.0, 10.0, 80.0, 24.0);
        let items = vec![DropdownItem::new("Option A"), DropdownItem::new("Option B")];
        dd.open(items, trigger, 100.0, &mut tree);

        assert!(dd.is_open());
        // Should anchor below trigger.
        assert!((dd.container_bounds.y - 34.0).abs() < 0.1);
        // Width should be at least trigger width or min_width.
        assert!(dd.container_bounds.width >= 100.0);
    }

    #[test]
    fn events_ignored_when_closed() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();

        let event = UIEvent::Click {
            node_id: 0,
            pos: Vec2::new(10.0, 10.0),
        };
        let result = dd.handle_event(&event, &mut tree);
        assert_eq!(result, None);
    }

    #[test]
    fn checked_items_show_checkmark() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = vec![
            DropdownItem::new("Normal"),
            DropdownItem::new("Checked").with_check(true),
        ];
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        let checked_id = dd.item_ids[1] as u32;
        let text = tree.get_node(checked_id).text.as_deref().unwrap();
        assert!(text.starts_with('\u{2713}'));
    }
}
