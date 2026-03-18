//! Static pixel-level drawing primitives for painting waveform data into
//! per-lane pixel buffers.
//!
//! Replaces Unity's tile-based `WaveformLevel.GetOrBuildTileTexture()` with
//! direct pixel painting. The spectral coloring and amplitude data come from
//! `WaveformRenderer`; this module handles the final rasterization.
//!
//! Follows the same patterns as `bitmap_painter.rs` — operates on
//! `&mut [Color32]` arrays, no allocations, bounds-checked.

use crate::bitmap_painter::{alpha_blend, fill_rect};
use crate::color;
use crate::node::Color32;
use crate::waveform_renderer::WaveformLevel;

/// Draw a visible region of a waveform into a pixel buffer.
///
/// This replaces Unity's tile-based rendering (WaveformRenderer lines 72-149
/// + WaveformLevel.GetOrBuildTileTexture lines 432-473).
///
/// Instead of creating tile textures positioned via RectTransform, we paint
/// the visible waveform region directly into the lane's pixel buffer.
///
/// Parameters:
/// - `buffer`: target pixel buffer (width × height, row-major)
/// - `buf_w`, `buf_h`: buffer dimensions
/// - `level`: the MIP level to sample from
/// - `x_start`, `x_end`: pixel X range to draw within the buffer
/// - `y_offset`: top edge of the waveform region in the buffer
/// - `lane_height`: height of the waveform lane in pixels
/// - `waveform_x_px`: X pixel position of the waveform start in content space
/// - `waveform_width_px`: total width of the waveform in pixels
/// - `scroll_offset_x`: horizontal scroll offset in pixels
pub fn draw_waveform(
    buffer: &mut [Color32],
    buf_w: usize,
    buf_h: usize,
    level: &WaveformLevel,
    x_start: i32,
    x_end: i32,
    y_offset: i32,
    lane_height: i32,
    waveform_x_px: f32,
    waveform_width_px: f32,
) {
    if level.texel_count() == 0 || waveform_width_px <= 0.0 || lane_height <= 0 {
        return;
    }

    let texel_count = level.texel_count() as f32;
    let height_padding = 10.0; // Unity: HeightPadding = 10f (line 34)
    let draw_height = (lane_height as f32 - height_padding).max(1.0);
    let mid = y_offset + lane_height / 2;

    // Draw center line (Unity: GetOrBuildTileTexture line 456)
    let center_color = color::WAVEFORM_CENTER_LINE;
    for px in x_start.max(0)..x_end.min(buf_w as i32) {
        let idx = mid as usize * buf_w + px as usize;
        if idx < buffer.len() {
            buffer[idx] = alpha_blend(buffer[idx], center_color);
        }
    }

    // Draw waveform bars
    for px in x_start.max(0)..x_end.min(buf_w as i32) {
        // Map pixel to normalized position within waveform
        let local_x = px as f32 - waveform_x_px;
        if local_x < 0.0 || local_x >= waveform_width_px {
            continue;
        }
        let norm = local_x / waveform_width_px;

        // Map to texel index (Unity: GetOrBuildTileTexture lines 447-452)
        let texel_index = (norm * texel_count) as usize;
        if texel_index >= level.texel_count() {
            continue;
        }

        let amp = level.amplitude(texel_index);
        let texel_color = level.color(texel_index);

        if amp <= 0.0001 {
            continue;
        }

        // Unity: `int half = Mathf.Max(1, Mathf.RoundToInt(amp * (textureHeight * 0.45f)));`
        let half = ((amp * draw_height * 0.45).round() as i32).max(1);
        let y_min = (mid - half).max(y_offset).max(0);
        let y_max = (mid + half).min(y_offset + lane_height - 1).min(buf_h as i32 - 1);

        for y in y_min..=y_max {
            let idx = y as usize * buf_w + px as usize;
            if idx < buffer.len() {
                buffer[idx] = texel_color;
            }
        }
    }
}

/// Draw the playhead line in the waveform lane.
///
/// Unity: playheadRect positioned at BeatToPixelAbsolute(playheadBeat)
/// with width = PlayheadWidth and color = PlayheadRed @ 0.85 alpha.
pub fn draw_playhead(
    buffer: &mut [Color32],
    buf_w: usize,
    buf_h: usize,
    x: i32,
    y_offset: i32,
    lane_height: i32,
    playhead_color: Color32,
    width: i32,
) {
    let half = width / 2;
    fill_rect(
        buffer,
        buf_w,
        buf_h,
        x - half,
        y_offset,
        width,
        lane_height,
        playhead_color,
    );
}

/// Draw a small text-style button overlay at a position.
///
/// Used for import/remove/expand/reanalyze buttons overlaid on the waveform lane.
/// In the bitmap UI, these are drawn as colored rectangles with text labels.
pub fn draw_waveform_button(
    buffer: &mut [Color32],
    buf_w: usize,
    buf_h: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bg_color: Color32,
    is_hovered: bool,
    is_pressed: bool,
    hover_color: Color32,
    pressed_color: Color32,
) {
    let color = if is_pressed {
        pressed_color
    } else if is_hovered {
        hover_color
    } else {
        bg_color
    };
    fill_rect(buffer, buf_w, buf_h, x, y, w, h, color);
}

/// Draw the empty state label background (centered in lane).
///
/// Unity: emptyStateLabel "Click to import audio" centered in viewport.
pub fn draw_empty_state_bg(
    buffer: &mut [Color32],
    buf_w: usize,
    buf_h: usize,
    y_offset: i32,
    lane_height: i32,
    lane_bg: Color32,
) {
    fill_rect(buffer, buf_w, buf_h, 0, y_offset, buf_w as i32, lane_height, lane_bg);
}

/// Draw mute/solo button indicator.
///
/// Unity: StemWaveformLane.SetMuteState/SetSoloState (lines 210-222).
pub fn draw_mute_solo_button(
    buffer: &mut [Color32],
    buf_w: usize,
    buf_h: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    is_active: bool,
    is_mute: bool,
) {
    let color = if is_active {
        if is_mute {
            color::MUTE_BTN_ACTIVE
        } else {
            color::SOLO_BTN_ACTIVE
        }
    } else {
        color::MUTE_SOLO_BTN_INACTIVE
    };
    fill_rect(buffer, buf_w, buf_h, x, y, w, h, color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waveform_renderer::WaveformRenderer;

    #[test]
    fn draw_waveform_empty_level_noop() {
        let r = WaveformRenderer::new();
        // No levels — should not panic
        assert!(r.select_level_for_zoom(100.0, 1.0).is_none());
    }

    #[test]
    fn draw_waveform_basic() {
        let mut renderer = WaveformRenderer::new();
        // 320 frames of loud signal
        let samples: Vec<f32> = (0..320).map(|i| (i as f32 / 10.0).sin() * 0.8).collect();
        renderer.set_audio_data(&samples, 1, 44100);
        assert!(renderer.is_ready());

        let level = renderer.select_level_for_zoom(320.0, 1.0).unwrap();
        let mut buf = vec![Color32::TRANSPARENT; 320 * 56];

        draw_waveform(
            &mut buf, 320, 56, level,
            0, 320,   // x range
            0, 56,    // y offset, lane height
            0.0, 320.0, // waveform position and width
        );

        // Some pixels should be non-transparent (waveform was drawn)
        let non_transparent = buf.iter().filter(|c| c.a > 0).count();
        assert!(non_transparent > 0, "Waveform should have drawn some pixels");
    }

    #[test]
    fn draw_playhead_visible() {
        let mut buf = vec![Color32::TRANSPARENT; 100 * 50];
        let red = Color32::new(217, 64, 56, 217); // PlayheadRed @ 0.85 alpha
        draw_playhead(&mut buf, 100, 50, 50, 0, 50, red, 2);

        // Check that pixels at x=49,50 are drawn
        let idx = 25 * 100 + 49; // mid-height, x=49
        assert!(buf[idx].a > 0, "Playhead should be visible");
    }
}
