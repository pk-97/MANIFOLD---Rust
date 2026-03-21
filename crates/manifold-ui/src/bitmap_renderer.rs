//! Per-layer bitmap renderer with dirty-checking. Manages a CPU pixel buffer
//! and paints all clips as colored rectangles into it.
//!
//! Mechanical translation of `Assets/Scripts/UI/Timeline/LayerBitmapRenderer.cs`.
//! GPU texture management lives in `manifold-renderer::layer_bitmap_gpu`.

use crate::bitmap_painter::{self, INSERT_CURSOR_COLOR, REGION_HIGHLIGHT_COLOR};
use crate::color;
use crate::node::Color32;
use crate::panels::viewport::{SelectionRegion, ViewportClip};
#[cfg(test)]
use manifold_core::ClipId;

// ── Constants (Unity LayerBitmapRenderer lines 47-48) ──

const MAX_TEXTURE_WIDTH: usize = 8192; // doubled from 4096 for 2x HiDPI headroom
const MIN_TEXTURE_WIDTH: usize = 4;

/// Minimum pixel width for a clip to be rendered (Unity UIConstants.MinClipRenderPx = 1).
const MIN_CLIP_RENDER_PX: i32 = 1;

/// Insert cursor width in logical pixels (Unity UIConstants.InsertCursorWidthPx = 2).
const INSERT_CURSOR_WIDTH_PX: i32 = 2;

/// Per-layer state passed to `repaint()` describing the current UI selection/hover state.
/// Avoids passing the full UIState struct across crate boundaries.
pub struct BitmapRepaintState<'a> {
    pub selection_version: u64,
    pub is_selected: &'a dyn Fn(&str) -> bool,
    pub hovered_clip_id: Option<&'a str>,
    pub has_region: bool,
    pub region: Option<&'a SelectionRegion>,
    pub has_insert_cursor: bool,
    pub insert_cursor_beat: f32,
    pub insert_cursor_layer: Option<usize>,
    pub pixels_per_beat: f32,
}

/// Manages a single CPU pixel buffer for one layer track.
/// Paints all clips as colored rectangles into a viewport-sized bitmap.
/// Replaces per-clip UITree nodes entirely.
pub struct LayerBitmapRenderer {
    layer_index: usize,

    // Pixel buffer (CPU side)
    pixel_buffer: Vec<Color32>,
    tex_w: usize,
    tex_h: usize,

    // Dirty-checking state (6 conditions, Unity lines 33-45)
    last_min_beat: f32,
    last_max_beat: f32,
    last_viewport_width: f32,
    last_selection_version: u64,
    last_clip_fingerprint: i32,
    last_had_selected_clips: bool,
    last_had_region_on_this_layer: bool,
    last_had_insert_cursor: bool,
    last_hover_on_this_layer: bool,
    last_hovered_clip_id: Option<String>,
    last_muted_state: bool,
    force_dirty: bool,

    // Repaint result flag
    was_dirty: bool,

    // Configuration
    render_scale: f32,
    track_height: f32,
    clip_vertical_padding: f32,
}

impl LayerBitmapRenderer {
    pub fn new(
        layer_index: usize,
        render_scale: f32,
        track_height: f32,
        clip_vertical_padding: f32,
    ) -> Self {
        Self {
            layer_index,
            pixel_buffer: Vec::new(),
            tex_w: 0,
            tex_h: 0,
            last_min_beat: f32::NAN,
            last_max_beat: f32::NAN,
            last_viewport_width: 0.0,
            last_selection_version: u64::MAX,
            last_clip_fingerprint: 0,
            last_had_selected_clips: false,
            last_had_region_on_this_layer: false,
            last_had_insert_cursor: false,
            last_hover_on_this_layer: false,
            last_hovered_clip_id: None,
            last_muted_state: false,
            force_dirty: true,
            was_dirty: false,
            render_scale,
            track_height,
            clip_vertical_padding,
        }
    }

    /// Force the next `repaint()` call to rebuild the texture.
    pub fn invalidate(&mut self) {
        self.force_dirty = true;
    }

    pub fn layer_index(&self) -> usize {
        self.layer_index
    }

    pub fn pixels(&self) -> &[Color32] {
        &self.pixel_buffer
    }

    pub fn tex_w(&self) -> usize {
        self.tex_w
    }

    pub fn tex_h(&self) -> usize {
        self.tex_h
    }

    /// Returns true if the last `repaint()` call actually repainted.
    pub fn was_dirty(&self) -> bool {
        self.was_dirty
    }

    /// Update render scale (e.g. on DPI change).
    pub fn set_render_scale(&mut self, scale: f32) {
        if (self.render_scale - scale).abs() > 0.001 {
            self.render_scale = scale;
            self.force_dirty = true;
        }
    }

    /// Update track height (e.g. on collapse/expand).
    pub fn set_track_height(&mut self, height: f32) {
        if (self.track_height - height).abs() > 0.1 {
            self.track_height = height;
            self.force_dirty = true;
        }
    }

    /// Repaint the layer bitmap if anything has changed.
    /// Returns true if the texture was actually repainted.
    ///
    /// Mechanical translation of Unity `LayerBitmapRenderer.Repaint()` (lines 91-272).
    pub fn repaint(
        &mut self,
        clips: &[ViewportClip],
        viewport_min_beat: f32,
        viewport_max_beat: f32,
        viewport_width_px: f32,
        is_muted: bool,
        time_sig_numerator: u32,
        state: &BitmapRepaintState,
    ) -> bool {
        self.was_dirty = false;

        if self.track_height <= 0.0 {
            return false;
        }

        // Compute texture dimensions (Unity lines 96-97)
        let tex_w = (viewport_width_px * self.render_scale)
            .round()
            .max(MIN_TEXTURE_WIDTH as f32)
            .min(MAX_TEXTURE_WIDTH as f32) as usize;
        let tex_h = (self.track_height * self.render_scale).round().max(2.0) as usize;

        // ── 6-condition dirty check (Unity lines 99-178) ──

        // 1. Viewport change
        let viewport_dirty = !approx_eq(self.last_min_beat, viewport_min_beat)
            || !approx_eq(self.last_max_beat, viewport_max_beat)
            || !approx_eq(self.last_viewport_width, viewport_width_px);

        // 2. Selection change (only evaluates on global version bump)
        let mut sel_dirty = false;
        if state.selection_version != self.last_selection_version {
            self.last_selection_version = state.selection_version;

            let mut has_selection = false;
            for clip in clips {
                if (state.is_selected)(&clip.clip_id) {
                    has_selection = true;
                    break;
                }
            }

            // Region overlay spans specific layers
            let region_on_this_layer = if state.has_region {
                if let Some(region) = state.region {
                    self.layer_index >= region.start_layer
                        && self.layer_index <= region.end_layer
                } else {
                    false
                }
            } else {
                false
            };

            // Insert cursor is per-layer
            let has_cursor = state.has_insert_cursor
                && state.insert_cursor_layer == Some(self.layer_index);

            sel_dirty = has_selection
                || self.last_had_selected_clips
                || region_on_this_layer
                || self.last_had_region_on_this_layer
                || has_cursor
                || self.last_had_insert_cursor;
            self.last_had_selected_clips = has_selection;
            self.last_had_region_on_this_layer = region_on_this_layer;
            self.last_had_insert_cursor = has_cursor;
        }

        // 3. Hover change (only evaluates on hoveredClipId change)
        let mut hover_dirty = false;
        let hov_id = state.hovered_clip_id;
        let last_hov = self.last_hovered_clip_id.as_deref();
        if last_hov != hov_id {
            self.last_hovered_clip_id = hov_id.map(String::from);

            let mut hover_on_this_layer = false;
            if let Some(hid) = hov_id {
                for clip in clips {
                    if clip.clip_id == hid {
                        hover_on_this_layer = true;
                        break;
                    }
                }
            }
            hover_dirty = hover_on_this_layer || self.last_hover_on_this_layer;
            self.last_hover_on_this_layer = hover_on_this_layer;
        }

        // 4. Clip data fingerprint
        let clip_fp = compute_clip_fingerprint(clips, viewport_min_beat, viewport_max_beat);
        let clip_data_dirty = self.last_clip_fingerprint != clip_fp;

        // 5. Mute state
        let mute_dirty = is_muted != self.last_muted_state;

        // 6. Force dirty (explicit invalidation)
        if !self.force_dirty
            && !viewport_dirty
            && !sel_dirty
            && !hover_dirty
            && !clip_data_dirty
            && !mute_dirty
        {
            return false;
        }

        // Update cached state
        self.last_min_beat = viewport_min_beat;
        self.last_max_beat = viewport_max_beat;
        self.last_viewport_width = viewport_width_px;
        self.last_clip_fingerprint = clip_fp;
        self.last_muted_state = is_muted;
        self.force_dirty = false;

        // Resize buffer if needed (Unity EnsureTexture, lines 427-444)
        self.ensure_buffer(tex_w, tex_h);

        // Clear to transparent (Unity lines 191-193)
        for p in self.pixel_buffer.iter_mut() {
            *p = Color32::TRANSPARENT;
        }

        let ppb = state.pixels_per_beat;
        let scaled_ppb = ppb * self.render_scale;

        // Paint grid lines BEFORE clips (Unity line 201)
        paint_grid_lines(
            &mut self.pixel_buffer,
            tex_w,
            tex_h,
            viewport_min_beat,
            ppb,
            scaled_ppb,
            time_sig_numerator,
        );

        // Clip vertical inset (Unity lines 204-207)
        let pad_px = (tex_h as f32 * self.clip_vertical_padding / self.track_height).round() as i32;
        let clip_y = pad_px;
        let clip_h = tex_h as i32 - pad_px * 2;

        // Paint clips on top of grid (Unity lines 210-232)
        for clip in clips {
            let end_beat = clip.start_beat + clip.duration_beats;
            if end_beat <= viewport_min_beat || clip.start_beat >= viewport_max_beat {
                continue;
            }

            // Compute pixel rect (Unity ComputeClipPixelRect, lines 297-308)
            let (x, w) = match compute_clip_pixel_rect(
                clip.start_beat,
                end_beat,
                viewport_min_beat,
                scaled_ppb,
                tex_w,
            ) {
                Some(v) => v,
                None => continue,
            };

            // Visual state
            let is_selected = (state.is_selected)(&clip.clip_id);
            let is_hovered = state.hovered_clip_id == Some(clip.clip_id.as_str());
            let clip_muted = is_muted || clip.is_muted;
            let is_locked = clip.is_locked;
            let is_generator = clip.is_generator;

            let bg = bitmap_painter::get_clip_color(
                is_selected,
                is_hovered,
                clip_muted,
                is_locked,
                is_generator,
            );
            let show_trim_hints = is_selected || is_hovered;
            bitmap_painter::draw_clip(
                &mut self.pixel_buffer,
                tex_w,
                tex_h,
                x,
                clip_y,
                w,
                clip_h,
                bg,
                is_selected,
                show_trim_hints,
                self.render_scale,
            );
        }

        // Paint region highlight ON TOP of clips (Unity lines 234-249)
        if state.has_region
            && let Some(region) = state.region
                && self.layer_index >= region.start_layer
                    && self.layer_index <= region.end_layer
                {
                    let region_start_px =
                        (region.start_beat - viewport_min_beat) * scaled_ppb;
                    let region_end_px = (region.end_beat - viewport_min_beat) * scaled_ppb;
                    let rx = region_start_px.round().max(0.0) as i32;
                    let rx1 = region_end_px.round().min(tex_w as f32) as i32;
                    let rw = rx1 - rx;
                    if rw > 0 {
                        bitmap_painter::fill_rect(
                            &mut self.pixel_buffer,
                            tex_w,
                            tex_h,
                            rx,
                            0,
                            rw,
                            tex_h as i32,
                            REGION_HIGHLIGHT_COLOR,
                        );
                    }
                }

        // Paint insert cursor line on the active layer only (Unity lines 251-260)
        if state.has_insert_cursor && state.insert_cursor_layer == Some(self.layer_index) {
            let cursor_px = (state.insert_cursor_beat - viewport_min_beat) * scaled_ppb;
            let cx = cursor_px.round() as i32;
            let cursor_w = (INSERT_CURSOR_WIDTH_PX as f32 * self.render_scale)
                .round()
                .max(1.0) as i32;
            if cx >= 0 && cx < tex_w as i32 {
                bitmap_painter::fill_rect(
                    &mut self.pixel_buffer,
                    tex_w,
                    tex_h,
                    cx,
                    0,
                    cursor_w,
                    tex_h as i32,
                    INSERT_CURSOR_COLOR,
                );
            }
        }

        self.was_dirty = true;
        true
    }

    /// Resize pixel buffer if needed (Unity EnsureTexture, lines 427-444).
    fn ensure_buffer(&mut self, w: usize, h: usize) {
        if self.tex_w == w && self.tex_h == h && self.pixel_buffer.len() == w * h {
            return;
        }
        self.tex_w = w;
        self.tex_h = h;
        let pixel_count = w * h;
        self.pixel_buffer.resize(pixel_count, Color32::TRANSPARENT);
    }
}

/// Lightweight fingerprint of visible clip state for this layer.
/// Changes when clips are added, removed, moved, resized, muted, or locked.
/// Unity: LayerBitmapRenderer.ComputeClipFingerprint (lines 331-344).
fn compute_clip_fingerprint(clips: &[ViewportClip], min_beat: f32, max_beat: f32) -> i32 {
    let mut hash = clips.len() as i32;
    for clip in clips {
        let end = clip.start_beat + clip.duration_beats;
        if end <= min_beat || clip.start_beat >= max_beat {
            continue;
        }
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(clip.start_beat.to_bits() as i32);
        hash = hash.wrapping_mul(31).wrapping_add(end.to_bits() as i32);
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(if clip.is_muted { 1 } else { 0 });
        hash = hash
            .wrapping_mul(31)
            .wrapping_add(if clip.is_locked { 1 } else { 0 });
    }
    hash
}

/// Compute the pixel X and width of a clip in the bitmap buffer.
/// Returns None if the clip is too narrow to render.
/// Unity: LayerBitmapRenderer.ComputeClipPixelRect (lines 297-308).
fn compute_clip_pixel_rect(
    start_beat: f32,
    end_beat: f32,
    viewport_min_beat: f32,
    scaled_ppb: f32,
    tex_w: usize,
) -> Option<(i32, i32)> {
    let start_px = (start_beat - viewport_min_beat) * scaled_ppb;
    let end_px = (end_beat - viewport_min_beat) * scaled_ppb;
    let x = start_px.round().max(0.0) as i32;
    let x1 = end_px.round().min(tex_w as f32) as i32;
    let w = x1 - x;
    if w >= MIN_CLIP_RENDER_PX {
        Some((x, w))
    } else {
        None
    }
}

/// Paint vertical grid lines into the bitmap buffer.
/// Matches GridOverlay's subdivision logic so clips occlude grid lines.
/// Unity: LayerBitmapRenderer.PaintGridLines (lines 352-425).
///
/// `logical_ppb`: logical pixels-per-beat (for subdivision threshold decisions).
/// `scaled_ppb`: texture pixels-per-beat (for pixel positioning in the buffer).
fn paint_grid_lines(
    buffer: &mut [Color32],
    tex_w: usize,
    tex_h: usize,
    viewport_min_beat: f32,
    logical_ppb: f32,
    scaled_ppb: f32,
    time_sig_numerator: u32,
) {
    if scaled_ppb < 1.0 || time_sig_numerator < 1 {
        return;
    }

    // Subdivision thresholds use logical ppb (matches GridOverlay exactly)
    let eighth_pixel_width = logical_ppb / 2.0;
    let sixteenth_pixel_width = logical_ppb / 4.0;
    let subdivisions_per_beat: i32 = if sixteenth_pixel_width >= 4.0 {
        4
    } else if eighth_pixel_width >= 6.0 {
        2
    } else {
        1
    };
    let show_beat_lines = logical_ppb >= 6.0;

    let bar_color = color::GRID_BAR_LINE;
    let beat_color = color::GRID_BEAT_LINE;
    let eighth_color = color::GRID_SUBDIVISION_LINE;
    let sixteenth_color = color::GRID_SIXTEENTH_LINE;

    // Walk subdivisions across viewport (use scaled ppb for pixel spacing)
    let subdiv_width = scaled_ppb / subdivisions_per_beat as f32;
    if subdiv_width < 1.0 {
        return;
    }

    // Find the first subdivision at or before viewport start
    let beats_per_bar = time_sig_numerator as f32;
    let step = 1.0 / subdivisions_per_beat as f32;
    let first_subdiv_beat =
        (viewport_min_beat * subdivisions_per_beat as f32).floor() / subdivisions_per_beat as f32;

    let mut subdiv_beat = first_subdiv_beat;
    loop {
        let px = (subdiv_beat - viewport_min_beat) * scaled_ppb;
        let col = px.round() as i32;
        if col >= tex_w as i32 {
            break;
        }
        if col < 0 {
            subdiv_beat += step;
            continue;
        }

        // Determine line type (Unity lines 386-413)
        let beat_in_bar =
            subdiv_beat - (subdiv_beat / beats_per_bar).floor() * beats_per_bar;
        let is_bar =
            beat_in_bar.abs() < 0.001 || (beat_in_bar - beats_per_bar).abs() < 0.001;
        let is_beat = (subdiv_beat - subdiv_beat.round()).abs() < 0.001;

        let (line_color, line_width) = if is_bar {
            (bar_color, 2.min(tex_w as i32 - col))
        } else if is_beat {
            if !show_beat_lines {
                subdiv_beat += step;
                continue;
            }
            (beat_color, 1)
        } else if subdivisions_per_beat == 4
            && ((subdiv_beat * 2.0) - (subdiv_beat * 2.0).round()).abs() < 0.001
        {
            (eighth_color, 1)
        } else {
            (sixteenth_color, 1)
        };

        // Paint vertical column — direct write (straight alpha).
        // Buffer is clear at this point; no need for AlphaBlend.
        // Unity lines 418-423.
        for lx in 0..line_width {
            let cx = col + lx;
            if cx >= tex_w as i32 {
                break;
            }
            let cx = cx as usize;
            for y in 0..tex_h {
                buffer[y * tex_w + cx] = line_color;
            }
        }

        subdiv_beat += step;
    }
}

/// Approximate float equality (matches Unity Mathf.Approximately).
#[inline]
fn approx_eq(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0001
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    fn make_clip(id: &str, start: f32, dur: f32) -> ViewportClip {
        ViewportClip {
            clip_id: ClipId::new(id),
            layer_index: 0,
            start_beat: start,
            duration_beats: dur,
            name: String::new(),
            color: color::CLIP_NORMAL,
            is_muted: false,
            is_locked: false,
            is_generator: false,
        }
    }

    fn make_state<'a>(sel_ver: u64) -> BitmapRepaintState<'a> {
        BitmapRepaintState {
            selection_version: sel_ver,
            is_selected: &|_| false,
            hovered_clip_id: None,
            has_region: false,
            region: None,
            has_insert_cursor: false,
            insert_cursor_beat: 0.0,
            insert_cursor_layer: None,
            pixels_per_beat: 100.0,
        }
    }

    #[test]
    fn dirty_check_skip_when_unchanged() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips = vec![make_clip("a", 0.0, 4.0)];
        let state = make_state(1);

        // First call: force_dirty → always repaints
        assert!(renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state));

        // Second call with same state → should skip
        assert!(!renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state));
    }

    #[test]
    fn dirty_check_triggers_on_viewport_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips = vec![make_clip("a", 0.0, 4.0)];
        let state = make_state(1);

        renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state);

        // Change viewport scroll → should repaint
        assert!(renderer.repaint(&clips, 1.0, 17.0, 800.0, false, 4, &state));
    }

    #[test]
    fn dirty_check_triggers_on_selection_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips = vec![make_clip("a", 0.0, 4.0)];
        let state1 = make_state(1);

        renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state1);

        // Bump selection version with a selected clip → should repaint
        let state2 = BitmapRepaintState {
            selection_version: 2,
            is_selected: &|id| id == "a",
            ..make_state(2)
        };
        assert!(renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state2));
    }

    #[test]
    fn dirty_check_triggers_on_hover_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips = vec![make_clip("a", 0.0, 4.0)];
        let state = make_state(1);

        renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state);

        // Hover a clip on this layer → should repaint
        let state2 = BitmapRepaintState {
            hovered_clip_id: Some("a"),
            ..make_state(1)
        };
        assert!(renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state2));
    }

    #[test]
    fn dirty_check_triggers_on_mute_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips = vec![make_clip("a", 0.0, 4.0)];
        let state = make_state(1);

        renderer.repaint(&clips, 0.0, 16.0, 800.0, false, 4, &state);

        // Toggle mute → should repaint
        assert!(renderer.repaint(&clips, 0.0, 16.0, 800.0, true, 4, &state));
    }

    #[test]
    fn dirty_check_triggers_on_clip_data_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips1 = vec![make_clip("a", 0.0, 4.0)];
        let state = make_state(1);

        renderer.repaint(&clips1, 0.0, 16.0, 800.0, false, 4, &state);

        // Add a clip → fingerprint changes → should repaint
        let clips2 = vec![make_clip("a", 0.0, 4.0), make_clip("b", 4.0, 4.0)];
        assert!(renderer.repaint(&clips2, 0.0, 16.0, 800.0, false, 4, &state));
    }

    #[test]
    fn fingerprint_stable_same_data() {
        let clips = vec![
            make_clip("a", 0.0, 4.0),
            make_clip("b", 8.0, 2.0),
        ];
        let fp1 = compute_clip_fingerprint(&clips, 0.0, 16.0);
        let fp2 = compute_clip_fingerprint(&clips, 0.0, 16.0);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_changes_on_move() {
        let clips1 = vec![make_clip("a", 0.0, 4.0)];
        let clips2 = vec![make_clip("a", 1.0, 4.0)];
        let fp1 = compute_clip_fingerprint(&clips1, 0.0, 16.0);
        let fp2 = compute_clip_fingerprint(&clips2, 0.0, 16.0);
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn texture_size_clamped() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0, 12.0);
        let clips = vec![];
        let state = make_state(1);

        // Very narrow viewport → clamped to MIN_TEXTURE_WIDTH
        renderer.repaint(&clips, 0.0, 1.0, 2.0, false, 4, &state);
        assert!(renderer.tex_w() >= MIN_TEXTURE_WIDTH);

        // Very wide viewport → clamped to MAX_TEXTURE_WIDTH
        renderer.invalidate();
        renderer.repaint(&clips, 0.0, 1.0, 10000.0, false, 4, &state);
        assert!(renderer.tex_w() <= MAX_TEXTURE_WIDTH);
    }

    #[test]
    fn clip_pixel_rect_basic() {
        let result = compute_clip_pixel_rect(2.0, 6.0, 0.0, 100.0, 1000);
        assert_eq!(result, Some((200, 400)));
    }

    #[test]
    fn clip_pixel_rect_clamps_to_bounds() {
        // Clip starts before viewport
        let result = compute_clip_pixel_rect(-2.0, 2.0, 0.0, 100.0, 1000);
        assert!(result.is_some());
        let (x, _w) = result.unwrap();
        assert_eq!(x, 0); // clamped to 0
    }

    #[test]
    fn clip_pixel_rect_too_narrow() {
        // Clip is < 1px wide at this zoom
        let result = compute_clip_pixel_rect(0.0, 0.001, 0.0, 10.0, 1000);
        assert!(result.is_none());
    }

    #[test]
    fn grid_lines_painted() {
        let mut buf = vec![Color32::TRANSPARENT; 400 * 20];
        paint_grid_lines(&mut buf, 400, 20, 0.0, 100.0, 100.0, 4);
        // At ppb=100, bar lines at beat 0 should be painted
        // Check pixel column 0 has something (bar line)
        assert_ne!(buf[0], Color32::TRANSPARENT);
    }
}
