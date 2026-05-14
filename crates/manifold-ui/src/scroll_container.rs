// Reusable scroll container primitive.
//
// Manages a scrollable viewport with clip-region-based content clipping,
// optional scrollbar, and reparenting support.
//
// Usage:
//   let mut sc = ScrollContainer::new();
//   let clip_id = sc.begin(tree, viewport_rect);
//   let start = tree.count();
//   // ... add child nodes ...
//   sc.set_content_height(total_content_h);
//   sc.reparent_content(tree, start);       // optional: reparent nodes under clip
//   sc.build_scrollbar(tree, sb_x, &cfg);   // optional: scrollbar
//   sc.update_scrollbar(tree);

use crate::node::*;
use crate::tree::UITree;

/// Scroll speed multiplier applied to incoming delta values.
/// The app layer already normalizes scroll delta (LineDelta × 20px),
/// so this is a final sensitivity multiplier, not a raw-to-pixel conversion.
pub const SCROLL_SPEED: f32 = 1.0;

/// Default scrollbar width.
pub const SCROLLBAR_W: f32 = 4.0;
/// Minimum scrollbar thumb height (prevents invisible thumb).
pub const SCROLLBAR_MIN_THUMB_H: f32 = 16.0;

/// Scrollbar visual configuration.
pub struct ScrollbarStyle {
    pub track_color: Color32,
    pub thumb_color: Color32,
    pub thumb_hover_color: Color32,
    pub corner_radius: f32,
}

pub struct ScrollContainer {
    /// Current scroll offset (pixels from top, 0 = top of content visible).
    scroll_offset: f32,
    /// Total content height (set after building content).
    content_height: f32,
    /// Viewport rect (set during begin()).
    viewport: Rect,
    /// The clip region node ID (set during begin()).
    clip_node_id: i32,
    /// Index into tree.nodes where content starts (for reparenting).
    content_start: usize,

    // Scrollbar state
    track_id: i32,
    thumb_id: i32,
}

impl ScrollContainer {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0.0,
            content_height: 0.0,
            viewport: Rect::ZERO,
            clip_node_id: -1,
            content_start: 0,
            track_id: -1,
            thumb_id: -1,
        }
    }

    /// Create the clip region node at the given viewport rect.
    /// Returns the clip node ID. Content built after this call can be
    /// reparented via `reparent_content()`, or parented directly using
    /// `clip_node_id()` as the parent.
    pub fn begin(&mut self, tree: &mut UITree, viewport_rect: Rect) -> i32 {
        self.viewport = viewport_rect;

        let clip_id = tree.add_node(
            -1,
            viewport_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        );

        self.clip_node_id = clip_id as i32;
        self.content_start = tree.count();
        clip_id as i32
    }

    /// Set the total content height (call after building all content).
    pub fn set_content_height(&mut self, height: f32) {
        self.content_height = height;
        self.clamp_scroll();
    }

    /// Reparent all nodes added since `begin()` under the clip region.
    /// Call this after building content if nodes were created with parent=-1.
    pub fn reparent_content(&self, tree: &mut UITree, start: usize) {
        let count = tree.count() - start;
        if count > 0 {
            tree.reparent_root_nodes(start, count, self.clip_node_id);
        }
    }

    // ── Scrollbar ──────────────────────────────────────────────────

    /// Build scrollbar track + thumb nodes at the given X position.
    /// The scrollbar spans the full viewport height. Call after building
    /// content and setting content_height.
    pub fn build_scrollbar(&mut self, tree: &mut UITree, x: f32, style: &ScrollbarStyle) {
        let vp = self.viewport;
        self.track_id = tree.add_button(
            -1,
            x,
            vp.y,
            SCROLLBAR_W,
            vp.height,
            UIStyle {
                bg_color: style.track_color,
                ..UIStyle::default()
            },
            "",
        ) as i32;
        self.thumb_id = tree.add_button(
            -1,
            x,
            vp.y,
            SCROLLBAR_W,
            SCROLLBAR_MIN_THUMB_H,
            UIStyle {
                bg_color: style.thumb_color,
                hover_bg_color: style.thumb_hover_color,
                corner_radius: style.corner_radius,
                ..UIStyle::default()
            },
            "",
        ) as i32;
        self.update_scrollbar(tree);
    }

    /// Update scrollbar thumb position and visibility from current scroll state.
    pub fn update_scrollbar(&self, tree: &mut UITree) {
        if self.track_id < 0 {
            return;
        }
        let vp_h = self.viewport.height;
        if self.content_height <= vp_h || vp_h <= 0.0 {
            tree.set_visible(self.track_id as u32, false);
            tree.set_visible(self.thumb_id as u32, false);
            return;
        }
        tree.set_visible(self.track_id as u32, true);
        tree.set_visible(self.thumb_id as u32, true);

        let ratio = vp_h / self.content_height;
        let thumb_h = (vp_h * ratio).max(SCROLLBAR_MIN_THUMB_H);
        let scroll_range = vp_h - thumb_h;
        let max = self.max_scroll();
        let scroll_frac = if max > 0.0 {
            self.scroll_offset / max
        } else {
            0.0
        };

        let thumb_y = self.viewport.y + scroll_frac * scroll_range;
        // Preserve thumb's X from its initial creation position.
        let thumb_x = tree.get_bounds(self.thumb_id as u32).x;
        tree.set_bounds(
            self.thumb_id as u32,
            Rect::new(thumb_x, thumb_y, SCROLLBAR_W, thumb_h),
        );
    }

    /// Convert a drag Y position to a scroll offset.
    /// Use this in scrollbar drag handlers.
    pub fn drag_to_scroll(&mut self, drag_y: f32) {
        let vp_h = self.viewport.height;
        let ratio = vp_h / self.content_height;
        let thumb_h = (vp_h * ratio).max(SCROLLBAR_MIN_THUMB_H);
        let scroll_range = vp_h - thumb_h;
        if scroll_range > 0.0 {
            let frac = ((drag_y - self.viewport.y) / scroll_range).clamp(0.0, 1.0);
            self.scroll_offset = frac * self.max_scroll();
        }
    }

    /// Scrollbar track node ID (for hit testing).
    pub fn track_id(&self) -> i32 {
        self.track_id
    }

    /// Scrollbar thumb node ID (for hit testing).
    pub fn thumb_id(&self) -> i32 {
        self.thumb_id
    }

    // ── Scroll state ───────────────────────────────────────────────

    /// Get the current scroll offset.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    /// Set the scroll offset directly (e.g., when syncing with another panel).
    pub fn set_scroll_offset(&mut self, offset: f32) {
        self.scroll_offset = offset;
        self.clamp_scroll();
    }

    /// Apply a scroll delta (from mouse wheel). Returns true if the offset changed.
    pub fn apply_scroll_delta(&mut self, delta: f32) -> bool {
        let old = self.scroll_offset;
        self.scroll_offset -= delta * SCROLL_SPEED;
        self.clamp_scroll();
        (self.scroll_offset - old).abs() > 0.01
    }

    /// Maximum scroll offset (content that extends below the viewport).
    pub fn max_scroll(&self) -> f32 {
        (self.content_height - self.viewport.height).max(0.0)
    }

    /// Whether the content overflows the viewport (scrollbar needed).
    pub fn can_scroll(&self) -> bool {
        self.content_height > self.viewport.height
    }

    /// The clip region node ID. Use as parent_id for scrolled content.
    pub fn clip_node_id(&self) -> i32 {
        self.clip_node_id
    }

    /// The viewport rect set during `begin()`.
    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// Compute the Y position for a content element at the given local offset.
    /// This accounts for the scroll offset so the element scrolls with the content.
    pub fn content_y(&self, local_offset: f32) -> f32 {
        self.viewport.y + local_offset - self.scroll_offset
    }

    /// Check if a content element at the given local Y + height is visible
    /// in the current viewport (for culling).
    pub fn is_visible(&self, local_y: f32, height: f32) -> bool {
        let screen_y = local_y - self.scroll_offset;
        let screen_bottom = screen_y + height;
        screen_bottom > 0.0 && screen_y < self.viewport.height
    }

    /// Scroll to ensure a content Y range is visible.
    pub fn scroll_to_reveal(&mut self, content_y: f32, height: f32) {
        if content_y < self.scroll_offset {
            self.scroll_offset = content_y;
        } else if content_y + height > self.scroll_offset + self.viewport.height {
            self.scroll_offset = content_y + height - self.viewport.height;
        }
        self.clamp_scroll();
    }

    /// Reset scroll to top.
    pub fn reset(&mut self) {
        self.scroll_offset = 0.0;
    }

    fn clamp_scroll(&mut self) {
        let max = self.max_scroll();
        self.scroll_offset = self.scroll_offset.clamp(0.0, max);
    }
}

impl Default for ScrollContainer {
    fn default() -> Self {
        Self::new()
    }
}
