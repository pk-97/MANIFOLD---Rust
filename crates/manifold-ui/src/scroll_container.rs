// Reusable scroll container primitive.
//
// Manages a scrollable viewport with clip-region-based content clipping.
// Matches Unity's BitmapScrollContainer pattern:
// 1. Creates a ClipRegion node at the viewport bounds
// 2. All scrolled content is parented under the clip node
// 3. Content Y positions are offset by -scroll_offset
// 4. The UITree's traversal + renderer automatically clip children
//    to the clip node's bounds via CLIPS_CHILDREN flag.
//
// Usage:
//   let mut sc = ScrollContainer::new();
//   let clip_id = sc.begin(tree, viewport_rect);
//   // ... add child nodes with parent_id = clip_id ...
//   sc.set_content_height(total_content_h);

use crate::node::*;
use crate::tree::UITree;

/// Scroll speed in logical pixels per normalized scroll unit.
/// Unity BitmapScrollContainer.cs divides raw delta by 120 then multiplies by 30.
/// winit LineDelta provides 1.0 per notch (already normalized), so 30.0 matches.
pub const SCROLL_SPEED: f32 = 30.0;

pub struct ScrollContainer {
    /// Current scroll offset (pixels from top, 0 = top of content visible).
    scroll_offset: f32,
    /// Total content height (set after building content).
    content_height: f32,
    /// Viewport height (set during begin()).
    viewport_height: f32,
    /// The clip region node ID (set during begin()).
    clip_node_id: i32,
}

impl ScrollContainer {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            clip_node_id: -1,
        }
    }

    /// Create the clip region node at the given viewport rect.
    /// Returns the node ID to use as parent_id for all scrolled content.
    /// Content should be positioned at Y = viewport_rect.y - scroll_offset + local_offset.
    pub fn begin(&mut self, tree: &mut UITree, viewport_rect: Rect) -> i32 {
        self.viewport_height = viewport_rect.height;

        // Create a ClipRegion node — the renderer will clip all descendants to this rect
        let clip_id = tree.add_node(
            -1, // root parent
            viewport_rect,
            UINodeType::ClipRegion,
            UIStyle::default(),
            None,
            UIFlags::VISIBLE | UIFlags::CLIPS_CHILDREN,
        );

        self.clip_node_id = clip_id as i32;
        clip_id as i32
    }

    /// Set the total content height (call after building all content).
    pub fn set_content_height(&mut self, height: f32) {
        self.content_height = height;
        // Clamp scroll offset
        self.clamp_scroll();
    }

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
        (self.content_height - self.viewport_height).max(0.0)
    }

    /// Whether the content overflows the viewport (scrollbar needed).
    pub fn can_scroll(&self) -> bool {
        self.content_height > self.viewport_height
    }

    /// The clip region node ID. Use as parent_id for scrolled content.
    pub fn clip_node_id(&self) -> i32 {
        self.clip_node_id
    }

    /// Compute the Y position for a content element at the given local offset.
    /// This accounts for the scroll offset so the element scrolls with the content.
    pub fn content_y(&self, viewport_top: f32, local_offset: f32) -> f32 {
        viewport_top + local_offset - self.scroll_offset
    }

    /// Check if a content element at the given local Y + height is visible
    /// in the current viewport (for culling).
    pub fn is_visible(&self, local_y: f32, height: f32) -> bool {
        let screen_y = local_y - self.scroll_offset;
        let screen_bottom = screen_y + height;
        screen_bottom > 0.0 && screen_y < self.viewport_height
    }

    /// Scroll to ensure a content Y range is visible.
    /// Port of Unity BitmapScrollContainer.ScrollToReveal.
    pub fn scroll_to_reveal(&mut self, content_y: f32, height: f32) {
        if content_y < self.scroll_offset {
            self.scroll_offset = content_y;
        } else if content_y + height > self.scroll_offset + self.viewport_height {
            self.scroll_offset = content_y + height - self.viewport_height;
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
