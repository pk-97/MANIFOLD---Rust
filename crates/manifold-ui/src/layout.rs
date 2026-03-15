use crate::node::Rect;
use crate::color;

/// Single source of truth for all top-level panel regions on screen.
///
/// Coordinate system: top-left origin (0,0 = top-left of screen).
/// All rects are in logical pixels.
///
/// Workflow: mutate the input properties (inspector width, split ratio, etc.),
/// then panels read the computed Rect properties to position themselves.
pub struct ScreenLayout {
    // ── Input properties ────────────────────────────────────────────
    pub screen_width: f32,
    pub screen_height: f32,
    pub transport_bar_height: f32,
    pub header_height: f32,
    pub footer_height: f32,
    pub inspector_width: f32,
    pub effect_browser_width: f32,
    /// Timeline height as fraction of content area. 0.30 = bottom 30%.
    /// Clamped to [0.15, 0.70] by resize handle.
    pub timeline_split_ratio: f32,
}

impl ScreenLayout {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            screen_width: width,
            screen_height: height,
            transport_bar_height: color::TRANSPORT_BAR_HEIGHT,
            header_height: color::HEADER_HEIGHT,
            footer_height: color::FOOTER_HEIGHT,
            inspector_width: 280.0,
            effect_browser_width: 0.0,
            timeline_split_ratio: 0.30,
        }
    }

    /// Resize to new screen dimensions.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.screen_width = width;
        self.screen_height = height;
    }

    // ── Derived ─────────────────────────────────────────────────────

    /// X offset for content panels (right of browser + inspector).
    pub fn content_left(&self) -> f32 {
        self.effect_browser_width + self.inspector_width
    }

    // ── Computed panel rects ────────────────────────────────────────

    /// Transport bar: full screen width, top edge.
    pub fn transport_bar(&self) -> Rect {
        Rect::new(0.0, 0.0, self.screen_width, self.transport_bar_height)
    }

    /// Content area below transport bar, right of sidebars.
    pub fn content_area(&self) -> Rect {
        let top = self.transport_bar_height;
        let h = self.screen_height - top;
        let left = self.content_left();
        Rect::new(left, top, self.screen_width - left, h)
    }

    /// Video container: upper portion of content area (above split line).
    pub fn video_area(&self) -> Rect {
        let content = self.content_area();
        let timeline_h = content.height * self.timeline_split_ratio;
        let video_h = content.height - timeline_h;
        Rect::new(content.x, content.y, content.width, video_h)
    }

    /// Full timeline region: lower portion of content area.
    /// Includes header, body, and footer.
    pub fn timeline_area(&self) -> Rect {
        let content = self.content_area();
        let timeline_h = content.height * self.timeline_split_ratio;
        let timeline_y = content.y + content.height - timeline_h;
        Rect::new(content.x, timeline_y, content.width, timeline_h)
    }

    /// Header bar: top of timeline area, full width.
    pub fn header(&self) -> Rect {
        let tl = self.timeline_area();
        Rect::new(tl.x, tl.y, tl.width, self.header_height)
    }

    /// Footer bar: bottom of timeline area, full width.
    pub fn footer(&self) -> Rect {
        let tl = self.timeline_area();
        Rect::new(tl.x, tl.y + tl.height - self.footer_height, tl.width, self.footer_height)
    }

    /// Timeline body: between header and footer within timeline area.
    pub fn timeline_body(&self) -> Rect {
        let tl = self.timeline_area();
        let top = tl.y + self.header_height;
        let bottom = tl.y + tl.height - self.footer_height;
        let h = (bottom - top).max(0.0);
        Rect::new(tl.x, top, tl.width, h)
    }

    /// Inspector sidebar: left edge, from below transport bar to bottom.
    pub fn inspector(&self) -> Rect {
        if self.inspector_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        Rect::new(
            self.effect_browser_width,
            top,
            self.inspector_width,
            self.screen_height - top,
        )
    }

    /// Effect browser: leftmost sidebar.
    pub fn effect_browser(&self) -> Rect {
        if self.effect_browser_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        Rect::new(0.0, top, self.effect_browser_width, self.screen_height - top)
    }

    // ── Convenience accessors for dropdown anchoring ──────────────

    /// Y position of the footer bar.
    pub fn footer_y(&self) -> f32 {
        self.footer().y
    }

    /// X position of the inspector sidebar.
    pub fn inspector_x(&self) -> f32 {
        self.inspector().x
    }

    /// Y position of the inspector sidebar.
    pub fn inspector_y(&self) -> f32 {
        self.inspector().y
    }

    /// Layer controls region: right side of timeline body.
    pub fn layer_controls(&self) -> Rect {
        let body = self.timeline_body();
        let w = color::LAYER_CONTROLS_WIDTH;
        Rect::new(body.x + body.width - w, body.y, w, body.height)
    }

    /// Timeline tracks region: timeline body minus layer controls.
    pub fn timeline_tracks(&self) -> Rect {
        let body = self.timeline_body();
        let ctrl_w = color::LAYER_CONTROLS_WIDTH;
        Rect::new(body.x, body.y, body.width - ctrl_w, body.height)
    }
}

impl Default for ScreenLayout {
    fn default() -> Self {
        Self::new(1280.0, 720.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_dimensions() {
        let layout = ScreenLayout::new(1920.0, 1080.0);

        let transport = layout.transport_bar();
        assert_eq!(transport.x, 0.0);
        assert_eq!(transport.y, 0.0);
        assert_eq!(transport.width, 1920.0);
        assert_eq!(transport.height, 36.0);
    }

    #[test]
    fn content_area_below_transport() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let content = layout.content_area();
        assert_eq!(content.x, 280.0); // right of default inspector
        assert_eq!(content.y, 36.0); // below transport
        assert_eq!(content.width, 1640.0); // 1920 - 280
        assert_eq!(content.height, 1044.0); // 1080 - 36
    }

    #[test]
    fn timeline_split() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let video = layout.video_area();
        let timeline = layout.timeline_area();

        // Video + timeline should fill content area
        let content = layout.content_area();
        assert!((video.height + timeline.height - content.height).abs() < 0.1);
    }

    #[test]
    fn header_footer_in_timeline() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let tl = layout.timeline_area();
        let header = layout.header();
        let footer = layout.footer();
        let body = layout.timeline_body();

        // Header at top of timeline
        assert_eq!(header.y, tl.y);
        // Footer at bottom of timeline
        assert!((footer.y + footer.height - tl.y - tl.height).abs() < 0.1);
        // Body between header and footer
        assert!((body.y - (header.y + header.height)).abs() < 0.1);
    }

    #[test]
    fn inspector_default_width() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let inspector = layout.inspector();
        assert_eq!(inspector.width, 280.0);
        assert_eq!(inspector.x, 0.0);
        assert_eq!(inspector.y, 36.0);
    }

    #[test]
    fn inspector_zero_when_closed() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 0.0;
        let inspector = layout.inspector();
        assert_eq!(inspector.width, 0.0);
    }

    #[test]
    fn inspector_pushes_content() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 300.0;
        let content = layout.content_area();
        assert_eq!(content.x, 300.0);
        assert_eq!(content.width, 1620.0);
    }
}
