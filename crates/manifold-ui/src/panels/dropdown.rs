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

use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse};
use super::popup_shell;
use super::PanelAction;
use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants ───────────────────────────────────────────────

// 24px rows / 8px padding read cramped
// once rows lost their per-item box (see the transparent-bg_color change in
// `build_nodes`) — 28px/12px gives labels room to breathe against the
// container edge and the hover/checked highlight.
const ITEM_HEIGHT: f32 = 28.0;
const PADDING_H: f32 = 12.0;
const PADDING_V: f32 = 4.0;
const MIN_WIDTH: f32 = 120.0;
const MAX_DROPDOWN_HEIGHT: f32 = 400.0;
const SCROLL_SPEED: f32 = 3.0;
const SEPARATOR_HEIGHT: f32 = 9.0; // pad + 1px line + pad
const CHAR_WIDTH: f32 = 7.0; // approximate glyph width for width estimation
/// Left inset for item label text inside its row, so it never sits flush
/// against the hover/checked highlight's rounded edge.
const ITEM_TEXT_INSET_X: f32 = 8.0;

/// A single item in a dropdown menu.
#[derive(Debug, Clone)]
pub struct DropdownItem {
    pub label: String,
    pub enabled: bool,
    pub separator_after: bool,
    /// Optional checkmark or other indicator.
    pub checked: bool,
    /// The [`PanelAction`] this item fires when chosen. When set, selecting the
    /// item returns [`DropdownAction::SelectedAction`] carrying it, so the app
    /// acts on it directly instead of mapping a positional index back to meaning
    /// through a parallel `Vec<Option<…>>` (the typed-dropdown direction, 2b.11).
    pub action: Option<PanelAction>,
}

impl DropdownItem {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            enabled: true,
            separator_after: false,
            checked: false,
            action: None,
        }
    }

    pub fn disabled(label: &str) -> Self {
        Self {
            label: label.to_string(),
            enabled: false,
            separator_after: false,
            checked: false,
            action: None,
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

    /// Attach the action this item fires when chosen (typed dropdown — see the
    /// `action` field).
    pub fn with_action(mut self, action: PanelAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// Actions returned by the dropdown panel.
/// (No `PartialEq` — `SelectedAction` carries a `PanelAction`, which isn't
/// comparable; callers `match` on the variant.)
#[derive(Debug, Clone)]
pub enum DropdownAction {
    /// User selected item at this index. (Untyped items — the app maps the index
    /// to meaning through its dropdown context.)
    Selected(usize),
    /// User selected a *typed* item — it carried its own [`PanelAction`], so the
    /// app fires it directly with no index→meaning map (2b.11).
    SelectedAction(PanelAction),
    /// User selected a color swatch at this index into the color grid.
    ColorSelected(usize),
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
    backdrop_id: Option<NodeId>,
    root_id: Option<NodeId>,
    item_ids: Vec<NodeId>,
    separator_ids: Vec<NodeId>,
    /// Index of currently hovered item (-1 = none).
    hovered_index: i32,
    /// Scroll offset in pixels (0 = top, positive = scrolled down).
    scroll_offset: f32,
    /// Total content height of all items (may exceed container height).
    content_height: f32,
    /// Viewport height for items (container height minus padding).
    viewport_height: f32,
    /// First node index in the tree (for node range checks).
    first_node: usize,
    node_count: usize,
    // ── Color grid (optional) ─────────────────────────────────
    /// Colors to render as a swatch grid below the text items.
    color_grid: Vec<Color32>,
    color_grid_cols: usize,
    color_swatch_ids: Vec<NodeId>,
    /// Action captured by `Overlay::on_event`, drained by the app-layer overlay
    /// driver. Selection lowering (`DropdownContext` → `PanelAction`) needs
    /// `UIRoot`'s cached device/resolution lists, so it stays app-side.
    pending_action: Option<DropdownAction>,
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
            backdrop_id: None,
            root_id: None,
            item_ids: Vec::new(),
            separator_ids: Vec::new(),
            hovered_index: -1,
            scroll_offset: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            first_node: 0,
            node_count: 0,
            color_grid: Vec::new(),
            color_grid_cols: 0,
            color_swatch_ids: Vec::new(),
            pending_action: None,
        }
    }

    /// Popups open instantly at full size/opacity (no
    /// enter/exit motion). Kept as a no-op so `UIRoot::update()` can still
    /// call it unconditionally every frame without special-casing.
    pub fn update(&mut self, _tree: &mut UITree) {}

    /// Drain the action captured since the last call (set by `Overlay::on_event`).
    /// The app lowers `Selected`/`ColorSelected` against its dropdown context.
    pub fn take_pending_action(&mut self) -> Option<DropdownAction> {
        self.pending_action.take()
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    /// Get the label of a menu item by index.
    pub fn item_label(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(|i| i.label.as_str())
    }

    /// Returns true when the dropdown is open and `pos` is inside its bounds.
    pub fn contains_point(&self, pos: Vec2) -> bool {
        self.is_open && self.container_bounds.contains(pos)
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }

    /// The dropdown container rect (for overlay occlusion).
    pub fn container_bounds(&self) -> Rect {
        self.container_bounds
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
        self.color_grid.clear();
        self.color_grid_cols = 0;
        let anchor = Vec2::new(trigger_rect.x, trigger_rect.y_max());
        self.open_at(items, anchor, min_width.max(trigger_rect.width), tree);
    }

    /// Open as a context menu at the given screen position.
    pub fn open_context(&mut self, items: Vec<DropdownItem>, pos: Vec2, tree: &mut UITree) {
        self.color_grid.clear();
        self.color_grid_cols = 0;
        self.open_at(items, pos, MIN_WIDTH, tree);
    }

    /// Open as a context menu with a color swatch grid below the text items.
    pub fn open_context_with_colors(
        &mut self,
        items: Vec<DropdownItem>,
        colors: Vec<Color32>,
        cols: usize,
        pos: Vec2,
        tree: &mut UITree,
    ) {
        self.color_grid = colors;
        self.color_grid_cols = cols;
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
        self.scroll_offset = 0.0;
        self.is_open = true;

        // Compute content dimensions.
        let content_width = self.compute_content_width();

        // If we have a color grid, ensure the menu is wide enough for it.
        let grid_width = if self.color_grid_cols > 0 {
            let swatch = color::COLOR_SWATCH_SIZE;
            let gap = color::COLOR_SWATCH_GAP;
            PADDING_H * 2.0 + self.color_grid_cols as f32 * (swatch + gap) - gap
        } else {
            0.0
        };
        let w = content_width.max(self.min_width).max(grid_width);

        // Compute full content height (all items, no cap).
        let mut items_h = 0.0f32;
        for item in &self.items {
            items_h += ITEM_HEIGHT;
            if item.separator_after {
                items_h += SEPARATOR_HEIGHT;
            }
        }
        self.content_height = items_h;

        let mut h = items_h + PADDING_V * 2.0;

        // Add color grid height.
        if !self.color_grid.is_empty() && self.color_grid_cols > 0 {
            let rows = self.color_grid.len().div_ceil(self.color_grid_cols);
            let swatch = color::COLOR_SWATCH_SIZE;
            let gap = color::COLOR_SWATCH_GAP;
            // Separator + grid rows + padding.
            h += SEPARATOR_HEIGHT + rows as f32 * (swatch + gap) - gap + PADDING_V;
        }

        // Cap height at MAX_DROPDOWN_HEIGHT, then clamp to screen.
        let h = h.min(MAX_DROPDOWN_HEIGHT).min(self.screen_height);
        self.viewport_height = h - PADDING_V * 2.0;

        // Edge clamping — clamp both position AND size to screen. Right/
        // bottom overflow is handled first (bottom tries flipping above the
        // anchor), then a final `.clamp(0.0, ...)` on BOTH axes catches the
        // left/top case too — an anchor that's itself off-screen (e.g. a
        // trigger scrolled partly past the left edge) used to leave `x`/`y`
        // negative here with nothing pulling them back on-screen.
        let w = w.min(self.screen_width);
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
        x = x.clamp(0.0, (self.screen_width - w).max(0.0));
        y = y.clamp(0.0, (self.screen_height - h).max(0.0));

        self.anchor = Vec2::new(x, y);
        self.container_bounds = Rect::new(x, y, w, h);

        // `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D3: `open_at` eagerly mints
        // the dropdown's nodes here (so it's visible the instant it opens,
        // not waiting for the next full `build_overlays()` cycle) — this runs
        // from event-handling code, outside `UIRoot::build_overlays()`'s own
        // `begin_region` bracket, so it needs its own. The LATER rebuild path
        // (`Overlay::build_at` -> `rebuild_nodes` -> `build_nodes`) runs
        // INSIDE that bracket already (`open_regions > 0`, so no assertion
        // risk either way) — this mints a second, nested region there (an
        // extra small container clipping to the same rect the outer one
        // already clips to). Harmless — one extra node against a dropdown's
        // handful, and the D4 structural test only cares that the tree
        // ROOT's children are all regions, not that regions never nest —
        // but not free, so a P3 cleanup could hoist this out if it matters.
        let region =
            tree.begin_region(self.container_bounds, crate::tree::ZTier::Overlay, "dropdown", UIFlags::empty());
        let region_start = tree.count();
        self.build_nodes(tree);
        tree.end_region(region, region_start);
    }

    /// Close the dropdown and hide all nodes.
    pub fn close(&mut self, tree: &mut UITree) {
        if !self.is_open {
            return;
        }
        self.is_open = false;
        if let Some(backdrop_id) = self.backdrop_id {
            tree.set_visible(backdrop_id, false);
        }
        if let Some(root_id) = self.root_id {
            tree.set_visible(root_id, false);
        }
    }

    /// Rebuild dropdown nodes after a tree clear (e.g., full UI rebuild).
    /// Only call when the dropdown is open.
    pub fn rebuild_nodes(&mut self, tree: &mut UITree) {
        self.build_nodes(tree);
    }

    fn build_nodes(&mut self, tree: &mut UITree) {
        self.first_node = tree.count();
        self.item_ids.clear();
        self.separator_ids.clear();
        self.color_swatch_ids.clear();

        // Popups appear instantly at full size/opacity (no
        // enter/exit motion). `bounds` is just the container rect.
        let bounds = self.container_bounds;

        // Scrim + bordered container via the shared shell. The §17 overlay loop
        // lifts the container with a soft drop-shadow (it skips the scrim).
        let shell = popup_shell::build(
            tree,
            (self.screen_width, self.screen_height),
            bounds,
            &popup_shell::PopupStyle::DROPDOWN,
        );
        self.backdrop_id = Some(shell.backdrop);
        self.root_id = Some(shell.container);

        // Build items — positions offset by scroll. Items outside the
        // viewport are created but hidden to preserve stable item_ids indices.
        let mut cy = PADDING_V - self.scroll_offset;
        let item_w = bounds.width - PADDING_H * 2.0;
        let viewport_top = PADDING_V;
        let viewport_bottom = PADDING_V + self.viewport_height;

        for i in 0..self.items.len() {
            let item = &self.items[i];
            let label = item.label.clone();
            let enabled = item.enabled;
            let checked = item.checked;
            let separator_after = item.separator_after;

            // Is this item visible in the viewport?
            let item_bottom = cy + ITEM_HEIGHT;
            let visible = item_bottom > viewport_top && cy < viewport_bottom;

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

            // unchecked rows are TRANSPARENT, not an
            // opaque per-row rect — the old DROPDOWN_BG fill on every row
            // created AA seams between rows ("rows with pixel gaps"). The
            // menu container is the only opaque surface; rows only paint
            // on hover/press/checked.
            let item_style = UIStyle {
                bg_color: if checked {
                    color::DROPDOWN_ITEM_SELECTED
                } else {
                    color::TRANSPARENT
                },
                hover_bg_color: if enabled {
                    color::DROPDOWN_HIGHLIGHT
                } else {
                    color::TRANSPARENT
                },
                pressed_bg_color: if enabled {
                    color::DROPDOWN_PRESSED_BG
                } else {
                    color::TRANSPARENT
                },
                text_color: if checked {
                    color::DROPDOWN_CHECK_COLOR
                } else {
                    text_color
                },
                font_size: color::FONT_BODY,
                text_align: TextAlign::Left,
                // MENU_ITEM_RADIUS, up from SMALL_RADIUS
                // (2.0) — the hover/checked highlight reads as a distinct
                // rounded chip against the flat container now that rows
                // aren't boxed.
                corner_radius: color::MENU_ITEM_RADIUS,
                text_inset_x: ITEM_TEXT_INSET_X,
                ..UIStyle::default()
            };

            let mut flags = if enabled {
                UIFlags::INTERACTIVE
            } else {
                UIFlags::DISABLED
            };
            if !visible {
                flags &= !UIFlags::INTERACTIVE;
            }

            let id = tree.add_node(
                self.root_id,
                Rect::new(bounds.x + PADDING_H, bounds.y + cy, item_w, ITEM_HEIGHT),
                UINodeType::Button,
                item_style,
                Some(&display_text),
                flags,
            );
            if !visible {
                tree.set_visible(id, false);
            }
            self.item_ids.push(id);
            cy += ITEM_HEIGHT;

            if separator_after {
                let sep_y = cy + SEPARATOR_HEIGHT / 2.0 - 0.5;
                let sep_visible = (sep_y + 1.0) > viewport_top && sep_y < viewport_bottom;
                let sep_style = UIStyle {
                    bg_color: color::DIVIDER_C32,
                    ..UIStyle::default()
                };
                let sep_id = tree.add_panel(
                    self.root_id,
                    bounds.x + PADDING_H,
                    bounds.y + sep_y,
                    item_w,
                    1.0,
                    sep_style,
                );
                if !sep_visible {
                    tree.set_visible(sep_id, false);
                }
                self.separator_ids.push(sep_id);
                cy += SEPARATOR_HEIGHT;
            }
        }

        // ── Color grid (optional) ──────────────────────────────
        if !self.color_grid.is_empty() && self.color_grid_cols > 0 {
            // Separator line above grid.
            let sep_y = cy + SEPARATOR_HEIGHT / 2.0 - 0.5;
            let sep_style = UIStyle {
                bg_color: color::DIVIDER_C32,
                ..UIStyle::default()
            };
            let sep_id = tree.add_panel(
                self.root_id,
                bounds.x + PADDING_H,
                bounds.y + sep_y,
                item_w,
                1.0,
                sep_style,
            );
            self.separator_ids.push(sep_id);
            cy += SEPARATOR_HEIGHT;

            let swatch = color::COLOR_SWATCH_SIZE;
            let gap = color::COLOR_SWATCH_GAP;
            let cols = self.color_grid_cols;

            for (i, &swatch_color) in self.color_grid.iter().enumerate() {
                let col = i % cols;
                let row = i / cols;
                let sx = bounds.x + PADDING_H + col as f32 * (swatch + gap);
                let sy = bounds.y + cy + row as f32 * (swatch + gap);

                let swatch_style = UIStyle {
                    bg_color: swatch_color,
                    hover_bg_color: color::lighten(swatch_color, 40),
                    pressed_bg_color: color::darken(swatch_color, 20),
                    corner_radius: color::SMALL_RADIUS,
                    border_width: 1.0,
                    border_color: Color32::new(0, 0, 0, 80),
                    ..UIStyle::default()
                };

                let id = tree.add_node(
                    self.root_id,
                    Rect::new(sx, sy, swatch, swatch),
                    UINodeType::Button,
                    swatch_style,
                    None,
                    UIFlags::INTERACTIVE,
                );
                self.color_swatch_ids.push(id);
            }
        }

        self.node_count = tree.count() - self.first_node;
    }

    /// The result of choosing item `index`: a typed [`DropdownAction::SelectedAction`]
    /// when the item carries its own action, else a positional `Selected(index)`
    /// for the app to map.
    fn select_result(&self, index: usize) -> DropdownAction {
        match self.items.get(index).and_then(|it| it.action.clone()) {
            Some(action) => DropdownAction::SelectedAction(action),
            None => DropdownAction::Selected(index),
        }
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
                // Check if clicked on one of our text items.
                if let Some(index) = self.item_index_for_node(*node_id) {
                    if self.items[index].enabled {
                        let result = self.select_result(index);
                        self.close(tree);
                        return Some(result);
                    }
                    // Clicked disabled item — consume but don't dismiss.
                    return Some(DropdownAction::Dismissed);
                }
                // Check if clicked on a color swatch.
                if let Some(index) = self.color_index_for_node(*node_id) {
                    self.close(tree);
                    return Some(DropdownAction::ColorSelected(index));
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
                if let Some(index) = self.item_index_for_node(*node_id)
                    && self.hovered_index == index as i32
                {
                    self.hovered_index = -1;
                }
                None
            }
            UIEvent::KeyDown { key, .. } => match key {
                crate::input::Key::Escape => {
                    self.close(tree);
                    Some(DropdownAction::Dismissed)
                }
                crate::input::Key::Enter => {
                    if self.hovered_index >= 0 {
                        let idx = self.hovered_index as usize;
                        if idx < self.items.len() && self.items[idx].enabled {
                            let result = self.select_result(idx);
                            self.close(tree);
                            return Some(result);
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
                // Type-ahead: a letter/number jumps the hover to the next
                // matching item (and steps through on repeat). Non-printable
                // keys fall through to a no-op.
                other => {
                    if let Some(ch) = other.to_char() {
                        self.type_ahead(ch);
                    }
                    None
                }
            },
            UIEvent::Scroll { delta, .. } => {
                if self.content_height > self.viewport_height {
                    self.scroll_offset = (self.scroll_offset - delta.y * SCROLL_SPEED)
                        .clamp(0.0, self.content_height - self.viewport_height);
                    self.build_nodes(tree);
                }
                // Always consume scroll while open so it doesn't propagate
                // to the viewport underneath.
                Some(DropdownAction::Dismissed)
            }
            // Consume right-clicks while open.
            UIEvent::RightClick { .. } => {
                self.close(tree);
                Some(DropdownAction::Dismissed)
            }
            // the dropdown used to eat
            // every drag event unconditionally here — the confirmed BUG-058
            // eater (an open dropdown swallowed a timeline drag's terminal
            // event, wedging move/trim). That arm is gone: a foreign drag
            // falls through to `_ => None` below (Ignored, not consumed) and
            // now dismisses the dropdown as a side effect of ownership
            // resolution (`UIRoot::resolve_drag_owner`) instead.
            _ => None,
        }
    }

    fn item_index_for_node(&self, node_id: NodeId) -> Option<usize> {
        self.item_ids.iter().position(|&id| id == node_id)
    }

    fn color_index_for_node(&self, node_id: NodeId) -> Option<usize> {
        self.color_swatch_ids.iter().position(|&id| id == node_id)
    }

    /// Type-ahead: move the hover to the next enabled item whose label starts
    /// with `ch`, wrapping. Searching from `hovered_index + 1` makes repeating
    /// the same key step through every match (standard list UX). Mirrors
    /// `move_hover`'s no-tree contract — the hover highlight repaints per frame.
    fn type_ahead(&mut self, ch: char) {
        if self.items.is_empty() {
            return;
        }
        let ch = ch.to_ascii_lowercase();
        let count = self.items.len();
        let start = if self.hovered_index >= 0 {
            self.hovered_index as usize + 1
        } else {
            0
        };
        for offset in 0..count {
            let idx = (start + offset) % count;
            let item = &self.items[idx];
            if item.enabled
                && item
                    .label
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with(ch)
            {
                self.hovered_index = idx as i32;
                self.ensure_hovered_visible();
                return;
            }
        }
    }

    fn move_hover(&mut self, direction: i32) {
        if self.items.is_empty() {
            return;
        }
        let count = self.items.len() as i32;
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
            if next >= 0 && (next as usize) < self.items.len() && self.items[next as usize].enabled
            {
                self.hovered_index = next;
                self.ensure_hovered_visible();
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

    /// Auto-scroll so the hovered item is visible in the viewport.
    fn ensure_hovered_visible(&mut self) {
        if self.hovered_index < 0 || self.content_height <= self.viewport_height {
            return;
        }
        // Compute the Y position of the hovered item in content space.
        let mut item_y = 0.0f32;
        for i in 0..self.hovered_index as usize {
            item_y += ITEM_HEIGHT;
            if self.items[i].separator_after {
                item_y += SEPARATOR_HEIGHT;
            }
        }
        let item_bottom = item_y + ITEM_HEIGHT;
        let max_scroll = self.content_height - self.viewport_height;

        // Scroll up if item is above viewport.
        if item_y < self.scroll_offset {
            self.scroll_offset = item_y.max(0.0);
        }
        // Scroll down if item is below viewport.
        if item_bottom > self.scroll_offset + self.viewport_height {
            self.scroll_offset = (item_bottom - self.viewport_height).min(max_scroll);
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

impl Overlay for DropdownPanel {
    fn is_open(&self) -> bool {
        self.is_open
    }

    fn modality(&self) -> Modality {
        // Floats; a click outside dismisses it (handled inside handle_event,
        // which returns Some(Dismissed) and self-closes).
        Modality::Modeless
    }

    fn anchor(&self) -> Anchor {
        // Positions itself from its stored anchor + screen size.
        Anchor::SelfManaged
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::ZERO
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        self.set_screen_size(placement.screen.x, placement.screen.y);
        self.rebuild_nodes(tree);
    }

    fn on_event(&mut self, event: &UIEvent, tree: &mut UITree) -> OverlayResponse {
        if !self.is_open {
            return OverlayResponse::Ignored;
        }
        match self.handle_event(event, tree) {
            // Selection/dismiss lowering happens app-side (needs UIRoot context
            // + caches); stash the raw action for the driver to drain.
            Some(action) => {
                self.pending_action = Some(action);
                OverlayResponse::Consumed(Vec::new())
            }
            // Hover / arrow-nav: not consumed → modeless fall-through.
            None => OverlayResponse::Ignored,
        }
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
        // 1 backdrop + 1 root + 4 items + 1 separator = 7 nodes
        assert_eq!(dd.node_count(), 7);
        assert!(tree.count() >= 7);
    }

    #[test]
    fn opening_appears_instantly_at_full_size_and_opacity() {
        // no enter/exit motion — a popup is fully sized
        // and fully opaque on the very first frame it opens, no tween to
        // settle over subsequent frames.
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        dd.open_context(make_items(), Vec2::new(100.0, 200.0), &mut tree);
        let container = tree.get_node(dd.root_id.unwrap()).unwrap();
        assert_eq!(
            container.style.bg_color.a,
            popup_shell::PopupStyle::DROPDOWN.bg.a,
            "container is at full opacity from the first opened frame"
        );
        assert_eq!(
            container.bounds.width, dd.container_bounds.width,
            "container is at full size from the first opened frame"
        );

        // `update()` is a no-op now (no tween to advance) — calling it
        // repeatedly must not change anything.
        dd.update(&mut tree);
        let container = tree.get_node(dd.root_id.unwrap()).unwrap();
        assert_eq!(container.style.bg_color.a, popup_shell::PopupStyle::DROPDOWN.bg.a);
    }

    #[test]
    fn long_menu_opened_near_screen_edge_stays_fully_on_screen() {
        // a long menu (the
        // MIDI note picker opens 128 items via this same `open()` path)
        // triggered near a screen edge ran off both the top/bottom AND the
        // left/right edges. A 40-item menu is plenty to blow past
        // MAX_DROPDOWN_HEIGHT and exercise the same edge-clamp + internal
        // scroll path.
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        let (screen_w, screen_h) = (1920.0, 1080.0);
        dd.set_screen_size(screen_w, screen_h);

        let items: Vec<DropdownItem> =
            (0..40).map(|i| DropdownItem::new(&format!("Item {i}"))).collect();

        // Trigger hugging the bottom-right corner — the pathological case
        // for both the vertical flip-above clamp and horizontal clamp.
        let trigger = Rect::new(screen_w - 40.0, screen_h - 24.0, 40.0, 24.0);
        dd.open(items, trigger, 120.0, &mut tree);

        let b = dd.container_bounds();
        assert!(b.x >= 0.0, "container left edge on-screen: x={}", b.x);
        assert!(b.y >= 0.0, "container top edge on-screen: y={}", b.y);
        assert!(
            b.x_max() <= screen_w + 0.01,
            "container right edge on-screen: x_max={} vs screen_w={}",
            b.x_max(),
            screen_w
        );
        assert!(
            b.y_max() <= screen_h + 0.01,
            "container bottom edge on-screen: y_max={} vs screen_h={}",
            b.y_max(),
            screen_h
        );
        // Content taller than the viewport scrolls internally rather than
        // growing the container past the screen.
        assert!(
            dd.content_height > dd.viewport_height,
            "40 items should overflow the capped viewport, exercising internal scroll"
        );

        // Same check, hugging the TOP-LEFT corner.
        let mut dd2 = DropdownPanel::new();
        dd2.set_screen_size(screen_w, screen_h);
        let items2: Vec<DropdownItem> =
            (0..40).map(|i| DropdownItem::new(&format!("Item {i}"))).collect();
        let trigger2 = Rect::new(0.0, 0.0, 40.0, 24.0);
        dd2.open(items2, trigger2, 120.0, &mut tree);
        let b2 = dd2.container_bounds();
        assert!(b2.x >= 0.0 && b2.y >= 0.0);
        assert!(b2.x_max() <= screen_w + 0.01);
        assert!(b2.y_max() <= screen_h + 0.01);

        // A trigger scrolled PARTLY OFF the top-left edge (negative anchor) —
        // the case the plain right/bottom clamp above missed before the
        // final `.clamp(0.0, ..)` was added.
        let mut dd3 = DropdownPanel::new();
        dd3.set_screen_size(screen_w, screen_h);
        let items3: Vec<DropdownItem> =
            (0..40).map(|i| DropdownItem::new(&format!("Item {i}"))).collect();
        let trigger3 = Rect::new(-30.0, -10.0, 40.0, 24.0);
        dd3.open(items3, trigger3, 120.0, &mut tree);
        let b3 = dd3.container_bounds();
        assert!(b3.x >= 0.0, "left edge on-screen despite negative anchor: x={}", b3.x);
        assert!(b3.y >= 0.0, "top edge on-screen despite negative anchor: y={}", b3.y);
        assert!(b3.x_max() <= screen_w + 0.01);
        assert!(b3.y_max() <= screen_h + 0.01);
    }

    #[test]
    fn click_item_selects_and_closes() {
        let mut tree = UITree::new();
        let mut dd = DropdownPanel::new();
        dd.set_screen_size(1920.0, 1080.0);

        let items = make_items();
        dd.open_context(items, Vec2::new(100.0, 200.0), &mut tree);

        let copy_id = dd.item_ids[1];
        let event = UIEvent::Click {
            node_id: copy_id,
            pos: Vec2::new(110.0, 230.0),
            modifiers: crate::input::Modifiers::default(),
        };
        let result = dd.handle_event(&event, &mut tree);
        assert!(matches!(result, Some(DropdownAction::Selected(1))));
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
            node_id: NodeId::PLACEHOLDER,
            pos: Vec2::new(500.0, 500.0),
            modifiers: crate::input::Modifiers::default(),
        };
        let result = dd.handle_event(&event, &mut tree);
        assert!(matches!(result, Some(DropdownAction::Dismissed)));
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
            node_id: NodeId::PLACEHOLDER,
            key: crate::input::Key::Escape,
            modifiers: crate::input::Modifiers {
                shift: false,
                ctrl: false,
                alt: false,
                command: false,
            },
        };
        let result = dd.handle_event(&event, &mut tree);
        assert!(matches!(result, Some(DropdownAction::Dismissed)));
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

        let disabled_id = dd.item_ids[1];
        let event = UIEvent::Click {
            node_id: disabled_id,
            pos: Vec2::new(110.0, 240.0),
            modifiers: crate::input::Modifiers::default(),
        };
        let result = dd.handle_event(&event, &mut tree);
        // Clicking disabled item dismisses but doesn't select.
        assert!(matches!(result, Some(DropdownAction::Dismissed)));
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
    fn type_ahead_jumps_and_steps_through_matches() {
        let mut dd = DropdownPanel::new();
        dd.items = vec![
            DropdownItem::new("Amplitude"),
            DropdownItem::new("Centroid"),
            DropdownItem::new("Noisiness"),
            DropdownItem::new("Flux"),
            DropdownItem::new("Transients"),
        ];
        dd.is_open = true;
        dd.hovered_index = -1;

        // 'c' → Centroid (index 1), case-insensitive.
        dd.type_ahead('C');
        assert_eq!(dd.hovered_index, 1);

        // 't' → Transients (index 4).
        dd.type_ahead('t');
        assert_eq!(dd.hovered_index, 4);

        // No match → hover unchanged.
        dd.type_ahead('z');
        assert_eq!(dd.hovered_index, 4);
    }

    #[test]
    fn type_ahead_repeats_cycle_through_same_prefix() {
        let mut dd = DropdownPanel::new();
        dd.items = vec![
            DropdownItem::new("Low"),
            DropdownItem::new("Mid"),
            DropdownItem::new("High"),
            DropdownItem::new("Master"),
        ];
        dd.is_open = true;
        dd.hovered_index = -1;

        // First 'm' → Mid (index 1).
        dd.type_ahead('m');
        assert_eq!(dd.hovered_index, 1);
        // Repeat 'm' → Master (index 3), stepping from after the current.
        dd.type_ahead('m');
        assert_eq!(dd.hovered_index, 3);
        // Repeat 'm' → wrap back to Mid (index 1).
        dd.type_ahead('m');
        assert_eq!(dd.hovered_index, 1);
    }

    #[test]
    fn type_ahead_skips_disabled() {
        let mut dd = DropdownPanel::new();
        dd.items = vec![
            DropdownItem::new("Sobel"),
            DropdownItem::disabled("Scharr"),
            DropdownItem::new("Sketch"),
        ];
        dd.is_open = true;
        dd.hovered_index = -1;

        // 's' → Sobel (0). Repeat skips disabled Scharr (1) → Sketch (2).
        dd.type_ahead('s');
        assert_eq!(dd.hovered_index, 0);
        dd.type_ahead('s');
        assert_eq!(dd.hovered_index, 2);
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
            node_id: NodeId::PLACEHOLDER,
            pos: Vec2::new(10.0, 10.0),
            modifiers: crate::input::Modifiers::default(),
        };
        let result = dd.handle_event(&event, &mut tree);
        assert!(result.is_none());
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

        let checked_id = dd.item_ids[1];
        let text = tree.get_node(checked_id).unwrap().text.as_deref().unwrap();
        assert!(text.starts_with('\u{2713}'));
    }
}
