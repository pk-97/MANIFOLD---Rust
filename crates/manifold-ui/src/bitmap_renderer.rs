//! Per-layer **grid** bitmap renderer with dirty-checking. Manages one CPU pixel
//! buffer holding the timeline grid lines (+ the layer-0 top separator) for a
//! single layer track.
//!
//! Since §24 5b the clip *bodies* are GPU rounded rects (`manifold-renderer::
//! clip_draw`), their *content* (audio waveforms) is per-clip GPU textures
//! (`clip_content_gpu`), and the timeline overlays (region highlight, insert
//! cursor, markers) are GPU rects emitted in the overlay pass. So this buffer
//! carries only the grid — a dense, full-width layer drawn UNDER the clip bodies,
//! showing through the gaps between them. Grid geometry is a pure function of the
//! viewport (scroll / zoom / width / time-sig), independent of selection, hover,
//! clip data, or mute — the dirty check is correspondingly small.
//!
//! GPU texture management lives in `manifold-renderer::layer_bitmap_gpu`.

use crate::color;
use crate::node::Color32;
use crate::panels::viewport::SelectionRegion;

// ── Constants (Unity LayerBitmapRenderer lines 47-48) ──

const MAX_TEXTURE_WIDTH: usize = 8192; // doubled from 4096 for 2x HiDPI headroom
const MIN_TEXTURE_WIDTH: usize = 4;

/// Manages a single CPU pixel buffer for one layer track's grid lines.
/// Drawn as a full-width quad UNDER the GPU clip bodies.
pub struct LayerBitmapRenderer {
    layer_index: usize,

    // One CPU pixel buffer: grid lines + the layer-0 top separator. Drawn BEFORE
    // the GPU clip pass, so opaque clip bodies occlude it and it shows through the
    // gaps. Clip bodies, waveforms, and overlays are all GPU now.
    pixel_buffer: Vec<Color32>,
    tex_w: usize,
    tex_h: usize,

    // Dirty-checking state. The grid depends only on the viewport + time-sig, so
    // this is just the viewport fingerprint plus an explicit force.
    last_min_beat: f32,
    last_max_beat: f32,
    last_viewport_width: f32,
    last_time_sig: u32,
    force_dirty: bool,

    // Repaint result flag
    was_dirty: bool,

    // Configuration
    render_scale: f32,
    track_height: f32,
}

impl LayerBitmapRenderer {
    pub fn new(layer_index: usize, render_scale: f32, track_height: f32) -> Self {
        Self {
            layer_index,
            pixel_buffer: Vec::new(),
            tex_w: 0,
            tex_h: 0,
            last_min_beat: f32::NAN,
            last_max_beat: f32::NAN,
            last_viewport_width: 0.0,
            last_time_sig: 0,
            force_dirty: true,
            was_dirty: false,
            render_scale,
            track_height,
        }
    }

    /// Force the next `repaint()` call to rebuild the texture.
    pub fn invalidate(&mut self) {
        self.force_dirty = true;
    }

    pub fn layer_index(&self) -> usize {
        self.layer_index
    }

    /// The grid buffer — uploaded to the layer texture, drawn before the GPU
    /// clip pass.
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

    /// Repaint the grid bitmap if the viewport changed.
    /// Returns true if the texture was actually repainted.
    pub fn repaint(
        &mut self,
        viewport_min_beat: f32,
        viewport_max_beat: f32,
        viewport_width_px: f32,
        time_sig_numerator: u32,
        pixels_per_beat: f32,
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

        // ── Dirty check ── The grid is a pure function of the viewport + time-sig.
        let viewport_dirty = !approx_eq(self.last_min_beat, viewport_min_beat)
            || !approx_eq(self.last_max_beat, viewport_max_beat)
            || !approx_eq(self.last_viewport_width, viewport_width_px);
        let timesig_dirty = time_sig_numerator != self.last_time_sig;

        if !self.force_dirty && !viewport_dirty && !timesig_dirty {
            return false;
        }

        let ppb = pixels_per_beat;
        let scaled_ppb = ppb * self.render_scale;

        // Detect scroll-only change (same zoom + width + time-sig, no force).
        let zoom_same = approx_eq(self.last_viewport_width, viewport_width_px)
            && approx_eq(
                self.last_max_beat - self.last_min_beat,
                viewport_max_beat - viewport_min_beat,
            );
        let scroll_only = viewport_dirty && zoom_same && !timesig_dirty && !self.force_dirty;

        // Update cached state
        let old_min_beat = self.last_min_beat;
        self.last_min_beat = viewport_min_beat;
        self.last_max_beat = viewport_max_beat;
        self.last_viewport_width = viewport_width_px;
        self.last_time_sig = time_sig_numerator;
        self.force_dirty = false;

        // Resize buffer if needed (Unity EnsureTexture, lines 427-444)
        self.ensure_buffer(tex_w, tex_h);

        // ── Pixel-shift scroll optimization ──
        // When only the scroll position changed, shift the existing pixel data and
        // repaint just the newly-exposed strip. The grid is full-width and dense,
        // so this keeps 4K auto-scroll upload bandwidth low.
        let mut strip_x: Option<(usize, usize)> = None; // (start_col, width) of dirty strip
        if scroll_only && self.tex_w == tex_w && self.tex_h == tex_h {
            let delta_beats = viewport_min_beat - old_min_beat;
            let delta_px = (delta_beats * scaled_ppb).round() as i32;
            let abs_delta = delta_px.unsigned_abs() as usize;

            if abs_delta > 0 && abs_delta < tex_w {
                shift_buffer(&mut self.pixel_buffer, tex_w, tex_h, delta_px, abs_delta);
                strip_x = Some(if delta_px > 0 {
                    (tex_w - abs_delta, abs_delta)
                } else {
                    (0, abs_delta)
                });
            }
        }

        // Full clear when we didn't pixel-shift.
        if strip_x.is_none() {
            for p in self.pixel_buffer.iter_mut() {
                *p = Color32::TRANSPARENT;
            }
        }

        // Paint range: full buffer on full repaint, exposed strip only on pixel-shift.
        let (paint_x0, paint_x1) = match strip_x {
            Some((start, width)) => (start, (start + width).min(tex_w)),
            None => (0, tex_w),
        };

        // Grid lines, then the layer-0 top separator. The grid sits BEHIND the GPU
        // clip bodies (opaque bodies occlude it; it shows through the gaps).
        paint_grid_lines(
            &mut self.pixel_buffer,
            tex_w,
            tex_h,
            viewport_min_beat,
            ppb,
            scaled_ppb,
            time_sig_numerator,
            paint_x0,
            paint_x1,
        );
        if self.layer_index == 0 {
            let sep_h = (color::TRACK_SEPARATOR_HEIGHT * self.render_scale)
                .round()
                .max(1.0) as usize;
            for y in 0..sep_h.min(tex_h) {
                let row = y * tex_w;
                for x in paint_x0..paint_x1 {
                    self.pixel_buffer[row + x] = color::SEPARATOR_COLOR;
                }
            }
        }

        self.was_dirty = true;
        true
    }

    /// Resize the pixel buffer if needed (Unity EnsureTexture, lines 427-444).
    fn ensure_buffer(&mut self, w: usize, h: usize) {
        let pixel_count = w * h;
        if self.tex_w == w && self.tex_h == h && self.pixel_buffer.len() == pixel_count {
            return;
        }
        self.tex_w = w;
        self.tex_h = h;
        self.pixel_buffer.resize(pixel_count, Color32::TRANSPARENT);
    }
}

/// Horizontal pixel-shift of one buffer for the scroll optimization: move rows
/// by `delta_px` (positive = scrolled right → shift left, expose the right
/// strip; negative = the mirror) and clear the newly-exposed strip. `abs_delta`
/// is `|delta_px|` and is assumed `0 < abs_delta < tex_w`.
fn shift_buffer(buffer: &mut [Color32], tex_w: usize, tex_h: usize, delta_px: i32, abs_delta: usize) {
    for y in 0..tex_h {
        let row = y * tex_w;
        if delta_px > 0 {
            buffer.copy_within(row + abs_delta..row + tex_w, row);
            for x in (tex_w - abs_delta)..tex_w {
                buffer[row + x] = Color32::TRANSPARENT;
            }
        } else {
            buffer.copy_within(row..row + tex_w - abs_delta, row + abs_delta);
            for x in 0..abs_delta {
                buffer[row + x] = Color32::TRANSPARENT;
            }
        }
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
    paint_x0: usize,
    paint_x1: usize,
) {
    if scaled_ppb < 1.0 || time_sig_numerator < 1 || paint_x0 >= paint_x1 {
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

    // At very zoomed-out levels, bar lines themselves become too dense.
    // Skip bars to maintain minimum spacing (adaptive multi-bar grid).
    let beats_per_bar = time_sig_numerator as f32;
    let bar_px = logical_ppb * beats_per_bar;
    let bar_skip: u32 = if bar_px >= 8.0 {
        1 // Show every bar
    } else if bar_px >= 4.0 {
        2 // Every 2 bars
    } else if bar_px >= 2.0 {
        4 // Every 4 bars
    } else {
        8 // Every 8 bars
    };

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
    let step = 1.0 / subdivisions_per_beat as f32;
    let first_subdiv_beat =
        (viewport_min_beat * subdivisions_per_beat as f32).floor() / subdivisions_per_beat as f32;

    let mut subdiv_beat = first_subdiv_beat;
    loop {
        let px = (subdiv_beat - viewport_min_beat) * scaled_ppb;
        let col = px.round() as i32;
        if col >= paint_x1 as i32 {
            break;
        }
        if col < 0 || (col as usize) < paint_x0 {
            subdiv_beat += step;
            continue;
        }

        // Determine line type (Unity lines 386-413)
        let beat_in_bar = subdiv_beat - (subdiv_beat / beats_per_bar).floor() * beats_per_bar;
        let is_bar = beat_in_bar.abs() < 0.001 || (beat_in_bar - beats_per_bar).abs() < 0.001;
        let is_beat = (subdiv_beat - subdiv_beat.round()).abs() < 0.001;

        let (line_color, line_width) = if is_bar {
            // At extreme zoom-out, skip intermediate bars
            if bar_skip > 1 {
                let bar_num = (subdiv_beat / beats_per_bar).round() as u32;
                if !bar_num.is_multiple_of(bar_skip) {
                    subdiv_beat += step;
                    continue;
                }
            }
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

        // Paint vertical column — direct write.
        for lx in 0..line_width {
            let cx = col + lx;
            if cx >= paint_x1 as i32 {
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

/// Whether a clip at `layer_index` spanning `[clip_start, clip_end)` falls inside
/// `region`. Mirrors `EditingService::get_clips_in_region` exactly: inclusive
/// layer range `[start_layer, end_layer]` and half-open beat overlap
/// (`clip_start < region.end && clip_end > region.start`). Keeping this identical
/// is what makes the marquee highlight WYSIWYG with what an op resolves. Public
/// so the GPU clip emitter (app_render) styles marquee-covered clips identically.
pub fn clip_overlaps_region(
    region: &SelectionRegion,
    layer_index: usize,
    clip_start: f32,
    clip_end: f32,
) -> bool {
    layer_index >= region.start_layer
        && layer_index <= region.end_layer
        && clip_start < region.end_beat.as_f32()
        && clip_end > region.start_beat.as_f32()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_foundation::Beats;

    #[test]
    fn clip_overlaps_region_matches_get_clips_in_region_semantics() {
        // Region [4,8) beats over layers 1..=3 — same bounds form as
        // EditingService::get_clips_in_region.
        let region = SelectionRegion {
            start_beat: Beats(4.0),
            end_beat: Beats(8.0),
            start_layer: 1,
            end_layer: 3,
        };
        // Inside: layer in range, beats overlap.
        assert!(clip_overlaps_region(&region, 2, 6.0, 7.0));
        // Half-open end: a clip starting exactly at end_beat is excluded.
        assert!(!clip_overlaps_region(&region, 2, 8.0, 12.0));
        // Half-open start: a clip ending exactly at start_beat is excluded.
        assert!(!clip_overlaps_region(&region, 2, 0.0, 4.0));
        // A clip ending just past start_beat is included.
        assert!(clip_overlaps_region(&region, 2, 3.0, 4.5));
        // Layer range inclusive at both ends.
        assert!(clip_overlaps_region(&region, 1, 6.0, 7.0));
        assert!(clip_overlaps_region(&region, 3, 6.0, 7.0));
        // Outside the layer range is excluded.
        assert!(!clip_overlaps_region(&region, 0, 6.0, 7.0));
        assert!(!clip_overlaps_region(&region, 4, 6.0, 7.0));
    }

    #[test]
    fn dirty_check_skip_when_unchanged() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0);

        // First call: force_dirty → always repaints
        assert!(renderer.repaint(0.0, 16.0, 800.0, 4, 100.0));

        // Second call with the same viewport → should skip
        assert!(!renderer.repaint(0.0, 16.0, 800.0, 4, 100.0));
    }

    #[test]
    fn dirty_check_triggers_on_viewport_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0);

        renderer.repaint(0.0, 16.0, 800.0, 4, 100.0);

        // Change viewport scroll → should repaint
        assert!(renderer.repaint(1.0, 17.0, 800.0, 4, 100.0));
    }

    #[test]
    fn dirty_check_triggers_on_time_sig_change() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0);

        renderer.repaint(0.0, 16.0, 800.0, 4, 100.0);

        // Same viewport but a new time signature → grid spacing changes → repaint.
        assert!(renderer.repaint(0.0, 16.0, 800.0, 3, 100.0));
    }

    #[test]
    fn texture_size_clamped() {
        let mut renderer = LayerBitmapRenderer::new(0, 1.0, 100.0);

        // Very narrow viewport → clamped to MIN_TEXTURE_WIDTH
        renderer.repaint(0.0, 1.0, 2.0, 4, 100.0);
        assert!(renderer.tex_w() >= MIN_TEXTURE_WIDTH);

        // Very wide viewport → clamped to MAX_TEXTURE_WIDTH
        renderer.invalidate();
        renderer.repaint(0.0, 1.0, 10000.0, 4, 100.0);
        assert!(renderer.tex_w() <= MAX_TEXTURE_WIDTH);
    }

    #[test]
    fn grid_lines_painted() {
        let mut buf = vec![Color32::TRANSPARENT; 400 * 20];
        paint_grid_lines(&mut buf, 400, 20, 0.0, 100.0, 100.0, 4, 0, 400);
        // At ppb=100, bar lines at beat 0 should be painted
        // Check pixel column 0 has something (bar line)
        assert_ne!(buf[0], Color32::TRANSPARENT);
    }
}
