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
    /// The clip region node ID (set during begin()). None until begin() runs.
    clip_node_id: Option<NodeId>,
    /// Index into tree.nodes where content starts (for reparenting).
    content_start: usize,
    /// One-past-the-last content node index (set during `reparent_content`).
    /// `[content_start, content_end)` is the reparented content, excluding the
    /// clip node (before) and the scrollbar (after) — the exact range an
    /// in-place scroll offsets. `content_start == content_end` ⇒ no content.
    content_end: usize,

    // Scrollbar state
    track_id: Option<NodeId>,
    thumb_id: Option<NodeId>,
}

impl ScrollContainer {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0.0,
            content_height: 0.0,
            viewport: Rect::ZERO,
            clip_node_id: None,
            content_start: 0,
            content_end: 0,
            track_id: None,
            thumb_id: None,
        }
    }

    /// Create the clip region node at the given viewport rect.
    /// Returns the clip node ID. Content built after this call can be
    /// reparented via `reparent_content()`, or parented directly using
    /// `clip_node_id()` as the parent.
    pub fn begin(&mut self, tree: &mut UITree, viewport_rect: Rect) -> NodeId {
        self.viewport = viewport_rect;

        let clip_id = tree.add_node(
            None,
            viewport_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        );

        self.clip_node_id = Some(clip_id);
        self.content_start = tree.count();
        clip_id
    }

    /// Set the total content height (call after building all content).
    pub fn set_content_height(&mut self, height: f32) {
        self.content_height = height;
        self.clamp_scroll();
    }

    /// Reparent all nodes added since `begin()` under the clip region.
    /// Call this after building content if nodes were created with parent=None.
    /// Records `[content_start, content_end)` so an in-place scroll can later
    /// offset exactly the content (scrollbar is built afterwards, so it's
    /// excluded).
    pub fn reparent_content(&mut self, tree: &mut UITree, start: usize) {
        self.content_start = start;
        self.content_end = tree.count();
        let count = self.content_end - start;
        if count > 0
            && let Some(clip_id) = self.clip_node_id {
                tree.reparent_root_nodes(start, count, clip_id);
            }
    }

    /// Shift every content node by `delta_y` in place — the cheap scroll path.
    /// Content nodes carry absolute bounds, so each is moved exactly once (no
    /// recursion); the clip node (the viewport) and scrollbar are untouched, so
    /// the content scrolls within the stable viewport. Returns false if there's
    /// no recorded content to move (caller should fall back to a rebuild).
    pub fn offset_content(&self, tree: &mut UITree, delta_y: f32) -> bool {
        if self.content_end <= self.content_start {
            return false;
        }
        for i in self.content_start..self.content_end {
            let id = tree.id_at(i);
            let mut b = tree.get_bounds(id);
            b.y += delta_y;
            tree.set_bounds(id, b);
        }
        true
    }

    // ── Scrollbar ──────────────────────────────────────────────────

    /// Build scrollbar track + thumb nodes at the given X position.
    /// The scrollbar spans the full viewport height. Call after building
    /// content and setting content_height.
    pub fn build_scrollbar(&mut self, tree: &mut UITree, x: f32, style: &ScrollbarStyle) {
        let vp = self.viewport;
        self.track_id = Some(tree.add_button(
            None,
            x,
            vp.y,
            SCROLLBAR_W,
            vp.height,
            UIStyle {
                bg_color: style.track_color,
                ..UIStyle::default()
            },
            "",
        ));
        self.thumb_id = Some(tree.add_button(
            None,
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
        ));
        self.update_scrollbar(tree);
    }

    /// Update scrollbar thumb position and visibility from current scroll state.
    pub fn update_scrollbar(&self, tree: &mut UITree) {
        let (Some(track_id), Some(thumb_id)) = (self.track_id, self.thumb_id) else {
            return;
        };
        let vp_h = self.viewport.height;
        if self.content_height <= vp_h || vp_h <= 0.0 {
            tree.set_visible(track_id, false);
            tree.set_visible(thumb_id, false);
            return;
        }
        tree.set_visible(track_id, true);
        tree.set_visible(thumb_id, true);

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
        let thumb_x = tree.get_bounds(thumb_id).x;
        tree.set_bounds(thumb_id, Rect::new(thumb_x, thumb_y, SCROLLBAR_W, thumb_h));
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

    /// Scrollbar track node ID (for hit testing). None until built.
    pub fn track_id(&self) -> Option<NodeId> {
        self.track_id
    }

    /// Scrollbar thumb node ID (for hit testing). None until built.
    pub fn thumb_id(&self) -> Option<NodeId> {
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
    /// None until `begin()` runs.
    pub fn clip_node_id(&self) -> Option<NodeId> {
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
