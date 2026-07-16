use crate::anim::AnimF32;
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
    /// Audio Setup dock width (D1). 0.0 = closed. A fold-out column pinned to
    /// the inspector's LEFT edge that expands leftward when opened, shrinking
    /// the content area (preview + timeline) — never the inspector, which stays
    /// right-anchored. One rule at every width: content shrinks, no overlay
    /// fallback (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` D1).
    pub audio_setup_width: f32,
    /// Scene Setup dock width (SCENE_SETUP_PANEL_DESIGN D2). 0.0 = closed. A
    /// fold-out column pinned to the inspector's LEFT edge — the exact same
    /// mechanism as `audio_setup_width` above, cloned per D2. The two utility
    /// columns are mutually exclusive (opening one animates the other closed);
    /// that toggle policy lives at the call site (`ui_root`/`app_render`), not
    /// here — `ScreenLayout` only knows how to lay out whatever widths it's
    /// given, same separation of concerns as the audio dock.
    pub scene_setup_width: f32,
    /// Timeline height as fraction of content area. 0.30 = bottom 30%.
    /// Clamped to [0.15, 0.70] by resize handle.
    pub timeline_split_ratio: f32,

    // ── P2 "panel-split snap-back" (D15) ────────────────────────────
    /// Double-click-to-default tweens for the two draggable splits above.
    /// Settled (not animating) except for the brief window right after a
    /// reset; `tick_splits` writes each eased value straight back into the
    /// field it mirrors (`inspector_width` / `timeline_split_ratio`) every
    /// frame it's in flight, so every existing consumer of those two fields —
    /// rendering, hit-testing (`is_near_split_handle`), persistence — sees the
    /// settling position automatically, with no call-site changes. A live
    /// drag still writes the field directly and instantly, exactly as
    /// before; only `reset_inspector_width`/`reset_timeline_split` touch
    /// these. Unlike a param's slider fill (a separate cosmetic widget next
    /// to an always-instant numeric readout), a split ratio has no second
    /// surface to keep instant — the field IS the visual position, so here
    /// the ease *is* the whole reset.
    inspector_width_anim: AnimF32,
    timeline_split_anim: AnimF32,
    /// D1 snap-back mirror for the Audio Setup dock width — same mirror-field
    /// pattern as `inspector_width_anim` above: `tick_splits` eases the reset
    /// straight back into `audio_setup_width` every animating frame, so every
    /// layout consumer sees the settling position with no call-site change.
    audio_setup_width_anim: AnimF32,
    /// D15 snap-back mirror for the Scene Setup dock width — same mirror-field
    /// pattern as `audio_setup_width_anim` above.
    scene_setup_width_anim: AnimF32,
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
            audio_setup_width: 0.0,
            scene_setup_width: 0.0,
            timeline_split_ratio: color::DEFAULT_TIMELINE_SPLIT_RATIO,
            inspector_width_anim: AnimF32::new(color::DEFAULT_INSPECTOR_WIDTH, color::MOTION_MED_MS)
                .with_curve(crate::anim::Curve::Snap),
            timeline_split_anim: AnimF32::new(color::DEFAULT_TIMELINE_SPLIT_RATIO, color::MOTION_MED_MS)
                .with_curve(crate::anim::Curve::Snap),
            audio_setup_width_anim: AnimF32::new(color::DEFAULT_AUDIO_SETUP_WIDTH, color::MOTION_MED_MS)
                .with_curve(crate::anim::Curve::Snap),
            scene_setup_width_anim: AnimF32::new(color::DEFAULT_SCENE_SETUP_WIDTH, color::MOTION_MED_MS)
                .with_curve(crate::anim::Curve::Snap),
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

    /// Working area below the transport bar, above the global footer, and LEFT
    /// of the full-height inspector — parent of the top region (preview) and the
    /// timeline. The transport (top) and footer (bottom) are full-width chrome
    /// that bracket this; the inspector bounds it on the right (and the effect
    /// browser, if any, on the left).
    pub fn content_area(&self) -> Rect {
        let top = self.transport_bar_height;
        let left = self.effect_browser_width;
        // The inspector (right column) AND whichever utility dock is open —
        // Audio Setup or Scene Setup, the two fold-out columns just left of
        // the inspector — bound the content on the right. Subtracting both
        // widths unconditionally is safe even though the two docks are meant
        // to be mutually exclusive (SCENE_SETUP_PANEL_DESIGN D2): the toggle
        // policy lives at the call site, not here, so `content_area` stays a
        // pure function of whatever widths it's handed — the same reasoning
        // that keeps this the whole reason preview + timeline shrink when a
        // dock opens (D1); `top_region`/`timeline_area`/`video_area` all read
        // `content_area()`, so they inherit it.
        let w = (self.screen_width
            - left
            - self.inspector_width
            - self.audio_setup_width
            - self.scene_setup_width)
            .max(0.0);
        let h = (self.screen_height - top - self.footer_height).max(0.0);
        Rect::new(left, top, w, h)
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

    /// Footer status bar: global full-width chrome pinned to the very bottom of
    /// the screen — the bottom counterpart to the transport bar. Spans the whole
    /// width (under the inspector too); the content area and inspector stop at
    /// its top edge.
    pub fn footer(&self) -> Rect {
        Rect::new(
            0.0,
            self.screen_height - self.footer_height,
            self.screen_width,
            self.footer_height,
        )
    }

    /// Timeline body: below the header, down to the bottom of the timeline area
    /// (which sits just above the global footer).
    pub fn timeline_body(&self) -> Rect {
        let tl = self.timeline_area();
        let top = tl.y + self.header_height;
        let h = (tl.height - self.header_height).max(0.0);
        Rect::new(tl.x, top, tl.width, h)
    }

    /// Inspector sidebar: a FULL-HEIGHT column against the right edge, from just
    /// below the transport bar down to the top of the global footer. The preview
    /// and the timeline share the content area to its left.
    pub fn inspector(&self) -> Rect {
        if self.inspector_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        let x = self.screen_width - self.inspector_width;
        let h = (self.screen_height - top - self.footer_height).max(0.0);
        Rect::new(x, top, self.inspector_width, h)
    }

    /// Audio Setup dock: a FULL-HEIGHT column pinned to the inspector's LEFT
    /// edge (D1). Same vertical extent as the inspector; its right edge is the
    /// inspector's left edge, and it expands leftward as `audio_setup_width`
    /// grows, pushing the content area. `Rect::ZERO` when closed
    /// (`audio_setup_width <= 0`), mirroring `inspector()`'s zero-guard so a
    /// closed dock is byte-identical to today's layout.
    pub fn audio_setup(&self) -> Rect {
        if self.audio_setup_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        let x = self.screen_width - self.inspector_width - self.audio_setup_width;
        let h = (self.screen_height - top - self.footer_height).max(0.0);
        Rect::new(x, top, self.audio_setup_width, h)
    }

    /// Scene Setup dock: a FULL-HEIGHT column pinned to the inspector's LEFT
    /// edge (SCENE_SETUP_PANEL_DESIGN D2) — cloned from [`Self::audio_setup`].
    /// Same slot: the two utility docks are mutually exclusive by construction
    /// at the call site (only one of `audio_setup_width`/`scene_setup_width`
    /// is ever non-zero at once), so pinning both to the inspector's left edge
    /// never produces an overlap in practice. `Rect::ZERO` when closed.
    pub fn scene_setup(&self) -> Rect {
        if self.scene_setup_width <= 0.0 {
            return Rect::ZERO;
        }
        let top = self.transport_bar_height;
        let x = self.screen_width - self.inspector_width - self.scene_setup_width;
        let h = (self.screen_height - top - self.footer_height).max(0.0);
        Rect::new(x, top, self.scene_setup_width, h)
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
        color::OVERVIEW_STRIP_HEIGHT + color::RULER_HEIGHT
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

    /// P2 "panel-split snap-back" (D15): double-click the video/timeline
    /// split handle to reset it to its default ratio, easing there instead
    /// of jumping. A no-op if already at the default.
    pub fn reset_timeline_split(&mut self) {
        let from = self.timeline_split_ratio;
        let target = color::DEFAULT_TIMELINE_SPLIT_RATIO;
        if from == target {
            return;
        }
        self.timeline_split_ratio = target;
        self.timeline_split_anim.snap(from);
        self.timeline_split_anim.set_target(target);
    }

    /// Same as [`Self::reset_timeline_split`], for the inspector-width split.
    pub fn reset_inspector_width(&mut self) {
        let from = self.inspector_width;
        let target = color::DEFAULT_INSPECTOR_WIDTH;
        if from == target {
            return;
        }
        self.inspector_width = target;
        self.inspector_width_anim.snap(from);
        self.inspector_width_anim.set_target(target);
    }

    /// Same as [`Self::reset_inspector_width`], for the Audio Setup dock width.
    /// Only reachable via a double-click on the dock's resize handle, which
    /// only exists while the dock is open — so this never resurrects a closed
    /// dock (D1: `audio_setup_width > 0` ⟺ open, and only the toggle drops it
    /// to 0; resize/snap-back move it only between non-zero widths).
    pub fn reset_audio_setup_width(&mut self) {
        let from = self.audio_setup_width;
        let target = color::DEFAULT_AUDIO_SETUP_WIDTH;
        if from == target {
            return;
        }
        self.audio_setup_width = target;
        self.audio_setup_width_anim.snap(from);
        self.audio_setup_width_anim.set_target(target);
    }

    /// Same as [`Self::reset_audio_setup_width`], for the Scene Setup dock
    /// width.
    pub fn reset_scene_setup_width(&mut self) {
        let from = self.scene_setup_width;
        let target = color::DEFAULT_SCENE_SETUP_WIDTH;
        if from == target {
            return;
        }
        self.scene_setup_width = target;
        self.scene_setup_width_anim.snap(from);
        self.scene_setup_width_anim.set_target(target);
    }

    /// Whether any split-reset tween is still in flight — for tests /
    /// automation harnesses driving the gesture headlessly.
    pub fn is_split_reset_animating(&self) -> bool {
        self.inspector_width_anim.is_animating()
            || self.timeline_split_anim.is_animating()
            || self.audio_setup_width_anim.is_animating()
            || self.scene_setup_width_anim.is_animating()
    }

    /// Advance both split-reset tweens by `dt_ms`; call once per frame
    /// alongside the app's other per-panel ticks. While either is animating,
    /// writes its eased value straight back into the field it mirrors so
    /// every layout consumer sees the settling position with no code change
    /// of its own. Returns whether either is still in flight.
    ///
    /// Checks `is_animating()` BEFORE calling `tick` (not the tick's own
    /// return value) so the settling frame — where `tick` flips to
    /// `false` — still gets its one last write-back; skipping it would
    /// leave the field one tick short of the exact target forever.
    pub fn tick_splits(&mut self, dt_ms: f32) -> bool {
        let inspector_was_animating = self.inspector_width_anim.is_animating();
        let inspector_still_animating = self.inspector_width_anim.tick(dt_ms);
        if inspector_was_animating {
            self.inspector_width = self.inspector_width_anim.value();
        }
        let timeline_was_animating = self.timeline_split_anim.is_animating();
        let timeline_still_animating = self.timeline_split_anim.tick(dt_ms);
        if timeline_was_animating {
            self.timeline_split_ratio = self.timeline_split_anim.value();
        }
        let audio_setup_was_animating = self.audio_setup_width_anim.is_animating();
        let audio_setup_still_animating = self.audio_setup_width_anim.tick(dt_ms);
        if audio_setup_was_animating {
            self.audio_setup_width = self.audio_setup_width_anim.value();
        }
        let scene_setup_was_animating = self.scene_setup_width_anim.is_animating();
        let scene_setup_still_animating = self.scene_setup_width_anim.tick(dt_ms);
        if scene_setup_was_animating {
            self.scene_setup_width = self.scene_setup_width_anim.value();
        }
        inspector_still_animating
            || timeline_still_animating
            || audio_setup_still_animating
            || scene_setup_still_animating
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
        assert_eq!(content.height, 1008.0); // 1080 - 36 transport - 36 footer
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
    fn header_in_timeline_footer_is_global_chrome() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let tl = layout.timeline_area();
        let header = layout.header();
        let footer = layout.footer();
        let body = layout.timeline_body();

        // Header at top of timeline.
        assert_eq!(header.y, tl.y);
        // Body runs from below the header to the bottom of the timeline area.
        assert!((body.y - (header.y + header.height)).abs() < 0.1);
        assert!((body.y + body.height - (tl.y + tl.height)).abs() < 0.1);
        // Footer is global full-width chrome pinned to the very bottom.
        assert_eq!(footer.x, 0.0);
        assert_eq!(footer.width, 1920.0);
        assert!((footer.y + footer.height - 1080.0).abs() < 0.1);
        // The timeline sits directly above the footer (no gap, no overlap).
        assert!((tl.y + tl.height - footer.y).abs() < 0.1);
    }

    #[test]
    fn inspector_is_full_height_right_column() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        let inspector = layout.inspector();
        assert_eq!(inspector.width, 500.0);
        assert_eq!(inspector.x, 1420.0); // 1920 - 500, against the right edge
        assert_eq!(inspector.y, 36.0); // below transport
        assert_eq!(inspector.height, 1008.0); // down to the global footer (1080 - 36 - 36)
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

    // ── Audio Setup dock column (D1) ─────────────────────────────────

    #[test]
    fn audio_setup_zero_when_closed() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        // Default layout: dock closed.
        assert_eq!(layout.audio_setup_width, 0.0);
        assert_eq!(layout.audio_setup(), Rect::ZERO);
    }

    #[test]
    fn audio_setup_zero_width_is_todays_layout_byte_identical() {
        // The zero-width-is-today's-layout invariant: with the dock closed,
        // content_area() and inspector() must be exactly what they were before
        // the dock existed (the values the pre-existing tests assert).
        let layout = ScreenLayout::new(1920.0, 1080.0);
        assert_eq!(layout.audio_setup_width, 0.0);
        let content = layout.content_area();
        assert_eq!(content.x, 0.0);
        assert_eq!(content.y, 36.0);
        assert_eq!(content.width, 1420.0); // 1920 - 500 inspector, dock adds nothing
        assert_eq!(content.height, 1008.0);
        let insp = layout.inspector();
        assert_eq!(insp.x, 1420.0);
        assert_eq!(insp.width, 500.0);
    }

    #[test]
    fn audio_setup_shrinks_both_preview_and_timeline() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        let preview_before = layout.video_area().width;
        let timeline_before = layout.timeline_area().width;
        let inspector_before = layout.inspector();
        // Open the dock: preview + timeline both narrow together, inspector
        // stays exactly where it was (right-anchored, never collapses — D1).
        layout.audio_setup_width = color::DEFAULT_AUDIO_SETUP_WIDTH;
        assert!(layout.video_area().width < preview_before);
        assert!(layout.timeline_area().width < timeline_before);
        assert_eq!(layout.inspector(), inspector_before);
    }

    #[test]
    fn audio_setup_sits_exactly_between_content_and_inspector() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.audio_setup_width = color::DEFAULT_AUDIO_SETUP_WIDTH;
        let dock = layout.audio_setup();
        let content = layout.content_area();
        let insp = layout.inspector();
        // Dock's left edge == content's right edge (no gap, no overlap).
        assert!((content.x + content.width - dock.x).abs() < 0.01);
        // Dock's right edge == inspector's left edge.
        assert!((dock.x + dock.width - insp.x).abs() < 0.01);
        // Full height, same as the inspector.
        assert_eq!(dock.y, insp.y);
        assert_eq!(dock.height, insp.height);
        assert_eq!(dock.width, color::DEFAULT_AUDIO_SETUP_WIDTH);
    }

    #[test]
    fn reset_audio_setup_width_snaps_data_and_starts_the_visual_ease() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.audio_setup_width = 640.0;

        layout.reset_audio_setup_width();
        // Data snaps instantly.
        assert_eq!(layout.audio_setup_width, color::DEFAULT_AUDIO_SETUP_WIDTH);
        assert!(layout.is_split_reset_animating(), "reset starts the visual ease");

        layout.tick_splits(color::MOTION_MED_MS * 0.5);
        assert_ne!(layout.audio_setup_width, 640.0, "width must have moved off the pre-reset value");
        assert!(layout.is_split_reset_animating(), "still mid-flight, not yet settled");

        for _ in 0..30 {
            layout.tick_splits(color::MOTION_MED_MS / 20.0);
        }
        assert!(!layout.is_split_reset_animating(), "tween settles");
        assert_eq!(layout.audio_setup_width, color::DEFAULT_AUDIO_SETUP_WIDTH);
    }

    // ── P2 "panel-split snap-back" (D15) ─────────────────────────────

    #[test]
    fn reset_inspector_width_snaps_data_and_starts_the_visual_ease() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.inspector_width = 700.0;

        layout.reset_inspector_width();
        // Data snaps instantly: the field already reads the default the
        // moment this returns.
        assert_eq!(layout.inspector_width, color::DEFAULT_INSPECTOR_WIDTH);
        assert!(layout.is_split_reset_animating(), "reset starts the visual ease");

        // Mid-flight: `tick_splits` writes the eased value back into the
        // field every animating frame, so it must have moved off the old
        // width — NOT necessarily monotonically toward the default, since
        // `Curve::Snap` overshoots by design (D15's back-out curve).
        layout.tick_splits(color::MOTION_MED_MS * 0.5);
        assert_ne!(layout.inspector_width, 700.0, "fill must have moved off the pre-reset width");
        assert!(layout.is_split_reset_animating(), "still mid-flight, not yet settled");

        for _ in 0..30 {
            layout.tick_splits(color::MOTION_MED_MS / 20.0);
        }
        assert!(!layout.is_split_reset_animating(), "tween settles");
        assert_eq!(layout.inspector_width, color::DEFAULT_INSPECTOR_WIDTH);
    }

    #[test]
    fn reset_timeline_split_snaps_data_and_starts_the_visual_ease() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.timeline_split_ratio = 0.6;

        layout.reset_timeline_split();
        assert_eq!(layout.timeline_split_ratio, color::DEFAULT_TIMELINE_SPLIT_RATIO);
        assert!(layout.is_split_reset_animating());

        layout.tick_splits(color::MOTION_MED_MS * 0.5);
        assert_ne!(layout.timeline_split_ratio, 0.6, "ratio must have moved off the pre-reset value");
        assert!(layout.is_split_reset_animating(), "still mid-flight, not yet settled");

        for _ in 0..30 {
            layout.tick_splits(color::MOTION_MED_MS / 20.0);
        }
        assert!(!layout.is_split_reset_animating());
        assert_eq!(layout.timeline_split_ratio, color::DEFAULT_TIMELINE_SPLIT_RATIO);
    }

    #[test]
    fn reset_already_at_default_is_a_no_op() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        // Fresh layout already sits at both defaults.
        layout.reset_inspector_width();
        layout.reset_timeline_split();
        assert!(!layout.is_split_reset_animating(), "no-op reset never starts a tween");
    }

    // ── Scene Setup dock column (SCENE_SETUP_PANEL_DESIGN D2) ─────────────

    #[test]
    fn scene_setup_zero_when_closed() {
        let layout = ScreenLayout::new(1920.0, 1080.0);
        assert_eq!(layout.scene_setup_width, 0.0);
        assert_eq!(layout.scene_setup(), Rect::ZERO);
    }

    #[test]
    fn scene_setup_zero_width_is_todays_layout_byte_identical() {
        // The zero-width-is-today's-layout invariant, mirroring the audio
        // dock's own gate: with BOTH utility docks closed, content_area()
        // and inspector() are exactly what they were before either dock
        // existed. This is the machine check §4's "Show path never pays"
        // invariant reduces to at the layout level.
        let layout = ScreenLayout::new(1920.0, 1080.0);
        assert_eq!(layout.audio_setup_width, 0.0);
        assert_eq!(layout.scene_setup_width, 0.0);
        let content = layout.content_area();
        assert_eq!(content.x, 0.0);
        assert_eq!(content.y, 36.0);
        assert_eq!(content.width, 1420.0); // 1920 - 500 inspector, neither dock adds anything
        assert_eq!(content.height, 1008.0);
        let insp = layout.inspector();
        assert_eq!(insp.x, 1420.0);
        assert_eq!(insp.width, 500.0);
    }

    #[test]
    fn scene_setup_shrinks_both_preview_and_timeline() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        let preview_before = layout.video_area().width;
        let timeline_before = layout.timeline_area().width;
        let inspector_before = layout.inspector();
        layout.scene_setup_width = color::DEFAULT_SCENE_SETUP_WIDTH;
        assert!(layout.video_area().width < preview_before);
        assert!(layout.timeline_area().width < timeline_before);
        assert_eq!(layout.inspector(), inspector_before);
    }

    #[test]
    fn scene_setup_sits_exactly_between_content_and_inspector() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.scene_setup_width = color::DEFAULT_SCENE_SETUP_WIDTH;
        let dock = layout.scene_setup();
        let content = layout.content_area();
        let insp = layout.inspector();
        assert!((content.x + content.width - dock.x).abs() < 0.01);
        assert!((dock.x + dock.width - insp.x).abs() < 0.01);
        assert_eq!(dock.y, insp.y);
        assert_eq!(dock.height, insp.height);
        assert_eq!(dock.width, color::DEFAULT_SCENE_SETUP_WIDTH);
    }

    #[test]
    fn reset_scene_setup_width_snaps_data_and_starts_the_visual_ease() {
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        layout.scene_setup_width = 600.0;

        layout.reset_scene_setup_width();
        assert_eq!(layout.scene_setup_width, color::DEFAULT_SCENE_SETUP_WIDTH);
        assert!(layout.is_split_reset_animating(), "reset starts the visual ease");

        layout.tick_splits(color::MOTION_MED_MS * 0.5);
        assert_ne!(layout.scene_setup_width, 600.0, "width must have moved off the pre-reset value");
        assert!(layout.is_split_reset_animating(), "still mid-flight, not yet settled");

        for _ in 0..30 {
            layout.tick_splits(color::MOTION_MED_MS / 20.0);
        }
        assert!(!layout.is_split_reset_animating(), "tween settles");
        assert_eq!(layout.scene_setup_width, color::DEFAULT_SCENE_SETUP_WIDTH);
    }

    #[test]
    fn audio_and_scene_setup_docks_are_mutually_exclusive_at_the_layout_level() {
        // ScreenLayout itself doesn't own the toggle policy (that lives at
        // the call site — D2), but both docks pin to the SAME slot (just
        // left of the inspector). With only one non-zero at a time (the
        // exclusive-toggle call site's actual invariant), content_area
        // narrows by exactly that dock's width — proving the geometry never
        // silently double-subtracts or overlaps across a toggle sequence.
        let mut layout = ScreenLayout::new(1920.0, 1080.0);
        let base_content = layout.content_area().width;

        layout.audio_setup_width = color::DEFAULT_AUDIO_SETUP_WIDTH;
        let with_audio = layout.content_area().width;
        assert!((base_content - with_audio - color::DEFAULT_AUDIO_SETUP_WIDTH).abs() < 0.01);

        // Toggle: audio closes, scene opens (the exclusive-toggle call site's
        // actual sequence).
        layout.audio_setup_width = 0.0;
        layout.scene_setup_width = color::DEFAULT_SCENE_SETUP_WIDTH;
        let with_scene = layout.content_area().width;
        assert!((base_content - with_scene - color::DEFAULT_SCENE_SETUP_WIDTH).abs() < 0.01);
    }
}
