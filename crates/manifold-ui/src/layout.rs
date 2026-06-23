use crate::color;
use crate::node::Rect;

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
    /// Whether the imported audio waveform lane is visible.
    pub waveform_lane_visible: bool,
    /// Whether the 4 stem lanes are expanded (visible below the master waveform).
    pub stem_lanes_expanded: bool,
}

impl ScreenLayout {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            screen_width: width,
            screen_height: height,
            transport_bar_height: color::TRANSPORT_BAR_HEIGHT,
            header_height: color::HEADER_HEIGHT,
            footer_height: color::FOOTER_HEIGHT,
            inspector_width: color::DEFAULT_INSPECTOR_WIDTH,
            effect_browser_width: 0.0,
            timeline_split_ratio: color::DEFAULT_TIMELINE_SPLIT_RATIO,
            waveform_lane_visible: false,
            stem_lanes_expanded: false,
        }
    }

    /// Resize to new screen dimensions.
    pub fn resize(&mut self, width: f32, height: f32) {
        self.screen_width = width;
        self.screen_height = height;
    }

    // ── Derived ─────────────────────────────────────────────────────

    /// X offset for the central content (right of the left effect browser).
    /// The inspector is a full-height column on the RIGHT, so it bounds the
    /// content's width (see [`content_area`]) but not its left edge.
    pub fn content_left(&self) -> f32 {
        self.effect_browser_width
    }

    // ── Computed panel rects ────────────────────────────────────────

    /// Transport bar: full screen width, top edge.
    pub fn transport_bar(&self) -> Rect {
        Rect::new(0.0, 0.0, self.screen_width, self.transport_bar_height)
    }

    /// Working area below the transport bar and LEFT of the full-height
    /// inspector — parent of the top region (preview) and the timeline. The
    /// inspector is a full-height right column, so it bounds this on the right
    /// (and the effect browser, if any, on the left).
    pub fn content_area(&self) -> Rect {
        let top = self.transport_bar_height;
        let left = self.effect_browser_width;
        let w = (self.screen_width - left - self.inspector_width).max(0.0);
        Rect::new(left, top, w, self.screen_height - top)
    }

    /// Top region: below the transport bar, above the timeline, content width
    /// (left of the full-height inspector). Holds the preview.
    pub fn top_region(&self) -> Rect {
        let content = self.content_area();
        let timeline_h = content.height * self.timeline_split_ratio;
        let h = (content.height - timeline_h).max(0.0);
        Rect::new(content.x, content.y, content.width, h)
    }

    /// Video container (preview): the whole top region — content width (the
    /// inspector is a separate full-height column to the right, and the timeline
    /// sits below).
    pub fn video_area(&self) -> Rect {
        self.top_region()
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
        Rect::new(
            tl.x,
            tl.y + tl.height - self.footer_height,
            tl.width,
            self.footer_height,
        )
    }

    /// Timeline body: between header and footer within timeline area.
    pub fn timeline_body(&self) -> Rect {
        let tl = self.timeline_area();
        let top = tl.y + self.header_height;
        let bottom = tl.y + tl.height - self.footer_height;
        let h = (bottom - top).max(0.0);
        Rect::new(tl.x, top, tl.width, h)
    }

    /// Inspector sidebar: a FULL-HEIGHT column against the right edge, from just
    /// below the transport bar down to the bottom of the screen. The preview and
    /// the timeline share the content area to its left.
    pub fn inspector(&self) -> Rect {
        if self.inspector_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        let x = self.screen_width - self.inspector_width;
        Rect::new(x, top, self.inspector_width, self.screen_height - top)
    }

    /// Effect browser: leftmost sidebar.
    pub fn effect_browser(&self) -> Rect {
        if self.effect_browser_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        Rect::new(
            0.0,
            top,
            self.effect_browser_width,
            self.screen_height - top,
        )
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

    /// Height of the non-scrollable header area above the track scroll container.
    /// Overview strip (16) + ruler (40) = 56px base.
    /// When audio waveform lane is visible: + 56px.
    /// When stem lanes are visible (4×56): + 224px.
    /// From Unity UIConstants: ImportedWaveformLaneHeight = 56, StemLaneHeight = 56.
    ///
    /// **INVARIANT**: This is the SINGLE SOURCE OF TRUTH for the header offset.
    /// Both `viewport.rs` (tracks_rect.y) and `layer_header.rs` (panel_origin.y)
    /// MUST use this method to compute the Y offset between the timeline body top
    /// and the scrollable track area. If they diverge, layer controls will be
    /// vertically misaligned with their tracks.
    pub fn track_header_height(&self) -> f32 {
        let mut h = color::OVERVIEW_STRIP_HEIGHT + color::RULER_HEIGHT;
        if self.waveform_lane_visible {
            h += color::WAVEFORM_LANE_HEIGHT;
        }
        if self.waveform_lane_visible && self.stem_lanes_expanded {
            h += 4.0 * color::STEM_LANE_HEIGHT;
        }
        h
    }

    /// Height of the imported audio waveform lane (when visible).
    /// From Unity UIConstants.ImportedWaveformLaneHeight = 56.
    pub fn waveform_lane_height() -> f32 {
        color::WAVEFORM_LANE_HEIGHT
    }

    /// Height of a single stem lane.
    /// From Unity UIConstants.StemLaneHeight = 56.
    pub fn stem_lane_height() -> f32 {
        color::STEM_LANE_HEIGHT
    }

    /// Layer controls region: LEFT side of timeline body — the track headers
    /// anchor each row (DAW/NLE convention), with the tracks scrolling to their
    /// right.
    pub fn layer_controls(&self) -> Rect {
        let body = self.timeline_body();
        let w = color::LAYER_CONTROLS_WIDTH;
        Rect::new(body.x, body.y, w, body.height)
    }

    /// Timeline tracks region: timeline body to the RIGHT of the layer controls.
    pub fn timeline_tracks(&self) -> Rect {
        let body = self.timeline_body();
        let ctrl_w = color::LAYER_CONTROLS_WIDTH;
        Rect::new(
            body.x + ctrl_w,
            body.y,
            (body.width - ctrl_w).max(0.0),
            body.height,
        )
    }

    /// Split handle rect: the boundary between video area and timeline area.
    /// From Unity PanelResizeHandle.cs — a thin horizontal bar the user can drag
    /// to adjust the video/timeline proportion.
    /// Handle height: 6px (same as InspectorResizeHandleWidth).
    ///
    /// Sits just *below* the seam (inside the timeline header), not centered on
    /// it. The preview is an opaque GPU blit drawn on top of the UI atlas and
    /// fills the video area down to `tl.y`; a handle straddling the seam would
    /// have its top half painted over. Keeping it on the UI side of the seam
    /// means the hover/drag highlight is always fully visible.
    pub fn split_handle(&self) -> Rect {
        let tl = self.timeline_area();
        let handle_h = color::INSPECTOR_RESIZE_HANDLE_WIDTH; // 6px
        Rect::new(tl.x, tl.y, tl.width, handle_h)
    }

    /// Check if a point is near the video/timeline split handle.
    pub fn is_near_split_handle(&self, pos: crate::node::Vec2) -> bool {
        self.split_handle().contains(pos)
    }

    /// Update the split ratio from a drag position (in screen Y).
    /// Clamps to [0.15, 0.70] matching Unity PanelResizeHandle min/max.
    /// From Unity PanelResizeHandle.OnDrag (lines 55-76).
    pub fn update_split_from_drag(&mut self, screen_y: f32) {
        let content = self.content_area();
        if content.height <= 0.0 {
            return;
        }
        // How much of the content area is below the drag point
        let timeline_h = (content.y + content.height) - screen_y;
        let ratio = (timeline_h / content.height).clamp(
            color::MIN_TIMELINE_SPLIT_RATIO,
            color::MAX_TIMELINE_SPLIT_RATIO,
        );
        self.timeline_split_ratio = ratio;
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
    fn content_area_below_transport_left_of_inspector() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let content = layout.content_area();
        assert_eq!(content.x, 0.0); // no effect browser
        assert_eq!(content.y, 36.0); // below transport
        assert_eq!(content.width, 1420.0); // 1920 - 500 inspector
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
    fn inspector_is_full_height_right_column() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let inspector = layout.inspector();
        assert_eq!(inspector.width, 500.0);
        assert_eq!(inspector.x, 1420.0); // 1920 - 500, against the right edge
        assert_eq!(inspector.y, 36.0); // below transport
        assert_eq!(inspector.height, 1044.0); // full height down to the bottom
        // Preview sits directly left of the inspector, no gap.
        let video = layout.video_area();
        assert!((video.x + video.width - inspector.x).abs() < 0.1);
        // The timeline also stops at the inspector's left edge.
        let timeline = layout.timeline_area();
        assert!((timeline.x + timeline.width - inspector.x).abs() < 0.1);
    }

    #[test]
    fn inspector_zero_when_closed() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 0.0;
        let inspector = layout.inspector();
        assert_eq!(inspector.width, 0.0);
    }

    #[test]
    fn inspector_shrinks_both_preview_and_timeline() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        // Both preview and timeline are content-width, left of the inspector.
        let tl = layout.timeline_area();
        assert_eq!(tl.x, 0.0);
        assert_eq!(tl.width, 1420.0); // 1920 - 500 inspector
        // Widening the full-height inspector shrinks the content column —
        // preview and timeline both narrow together.
        let preview_before = layout.video_area().width;
        let timeline_before = layout.timeline_area().width;
        layout.inspector_width = 600.0;
        assert!(layout.video_area().width < preview_before);
        assert!(layout.timeline_area().width < timeline_before);
    }

    #[test]
    fn layer_controls_on_left_tracks_on_right() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let body = layout.timeline_body();
        let controls = layout.layer_controls();
        let tracks = layout.timeline_tracks();
        // Controls anchor the left edge of the timeline body.
        assert_eq!(controls.x, body.x);
        assert_eq!(controls.width, color::LAYER_CONTROLS_WIDTH);
        // Tracks begin immediately to the right of the controls, no gap/overlap.
        assert!((tracks.x - (controls.x + controls.width)).abs() < 0.1);
        assert!((tracks.x + tracks.width - (body.x + body.width)).abs() < 0.1);
    }
}
