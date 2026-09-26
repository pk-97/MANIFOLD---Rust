//! Static pixel-level drawing primitives for painting waveform data into
//! per-lane pixel buffers.
//!
//! Draws the cached low/mid/high energy envelopes inside the original signal's
//! peak silhouette. All analysis happens in `WaveformRenderer` at load time.
//!
//! Follows the same patterns as `bitmap_painter.rs` — operates on
//! `&mut [Color32]` arrays, no allocations, bounds-checked.

use crate::bitmap_painter::fill_rect;
use crate::color;
use crate::node::Color32;
use crate::waveform_renderer::WaveformLevel;

/// The on-screen pixel X range `[x_start, x_end)` a waveform occupies, clamped to
/// the buffer. `left_px` is the (scroll-adjusted) left edge in content space.
/// Shared by the waveform lane and the stem lanes, which computed it identically.
pub fn visible_x_range(left_px: f32, width_px: f32, buf_w: i32) -> (i32, i32) {
    let draw_left = left_px as i32;
    let draw_right = ((left_px + width_px) as i32).min(buf_w);
    (draw_left.max(0), draw_right.min(buf_w))
}

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
/// - `src_start`, `src_end`: the source-file sub-window to show, as fractions of
///   the file in `[0, 1]`. The clip is a *window* onto the file (Ableton model):
///   `src_start..src_end` of the file's texels map across `waveform_width_px`, so
///   trimming the clip reveals more/less of the file instead of rescaling it.
///   Pass `(0.0, 1.0)` to show the whole file.
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
    src_start: f32,
    src_end: f32,
) {
    if level.texel_count() == 0 || waveform_width_px <= 0.0 || lane_height <= 0 {
        return;
    }

    let src_span = (src_end as f64 - src_start as f64).max(0.0);
    let mid = y_offset as f32 + lane_height as f32 * 0.5;
    // Proportional padding stays consistent at Retina scale and in short clips.
    let max_half_height = lane_height as f32 * 0.43;

    for px in x_start.max(0)..x_end.min(buf_w as i32) {
        let left = (px as f64 - waveform_x_px as f64).max(0.0);
        let right = (px as f64 + 1.0 - waveform_x_px as f64).min(waveform_width_px as f64);
        if right <= left {
            continue;
        }
        // Pool the complete time interval under this pixel, including the last
        // partial source bin. Point sampling used to miss narrow peaks.
        let sample = level.sample_range(
            src_start as f64 + left / waveform_width_px as f64 * src_span,
            src_start as f64 + right / waveform_width_px as f64 * src_span,
        );
        if sample.peak <= 0.0 {
            continue;
        }
        let half = sample.peak.clamp(0.0, 1.0).powf(0.7) * max_half_height;
        // RMS supplies the sustained body; a smaller peak contribution keeps
        // short percussive bands visible in an overview. Nest contributions so
        // a loud bass envelope cannot completely cover a quieter high band.
        let weights: [f32; 3] = std::array::from_fn(|band| {
            0.75 * sample.band_rms[band] + 0.25 * sample.band_peaks[band]
        });
        let total: f32 = weights.iter().sum();
        if total <= 0.0 {
            continue;
        }
        let high_half = half * weights[2] / total;
        let mid_high_half = half * (weights[1] + weights[2]) / total;
        let y_min = (mid - half).floor().max(y_offset as f32).max(0.0) as i32;
        let y_max = (mid + half)
            .ceil()
            .min((y_offset + lane_height) as f32)
            .min(buf_h as f32) as i32;
        let x_coverage = (right - left).clamp(0.0, 1.0) as f32;
        for y in y_min..y_max {
            let idx = y as usize * buf_w + px as usize;
            if idx < buffer.len() {
                // Integrate vertical pixel coverage at both the outer edge and
                // band boundaries. Use straight alpha (the clip texture's GPU
                // blend contract), not the bitmap painter's opaque blending.
                // Work relative to the centre so cropping a clip cannot change
                // edge rounding through subtraction at a different Y origin.
                let pixel_from_mid = y as f32 - mid;
                let cover = |h: f32| interval_coverage(-h, h, pixel_from_mid);
                let outer = cover(half);
                let inner_mid = cover(mid_high_half).min(outer);
                let inner_high = cover(high_half).min(inner_mid);
                let mut src = if inner_high >= 1.0 {
                    color::WAVEFORM_HIGH
                } else if inner_mid >= 1.0 && inner_high == 0.0 {
                    color::WAVEFORM_MID
                } else if outer >= 1.0 && inner_mid == 0.0 {
                    color::WAVEFORM_LOW
                } else {
                    let areas = [outer - inner_mid, inner_mid - inner_high, inner_high];
                    let palette = [
                        color::WAVEFORM_LOW,
                        color::WAVEFORM_MID,
                        color::WAVEFORM_HIGH,
                    ];
                    let channel = |get: fn(Color32) -> u8| {
                        (areas
                            .iter()
                            .zip(palette)
                            .map(|(a, c)| a * get(c) as f32)
                            .sum::<f32>()
                            / outer.max(f32::EPSILON))
                        .round() as u8
                    };
                    Color32::new(channel(|c| c.r), channel(|c| c.g), channel(|c| c.b), 255)
                };
                src.a = (outer * x_coverage * 255.0).round() as u8;
                buffer[idx] = straight_alpha_over(buffer[idx], src);
            }
        }
    }
}

fn interval_coverage(start: f32, end: f32, pixel: f32) -> f32 {
    (end.min(pixel + 1.0) - start.max(pixel)).clamp(0.0, 1.0)
}

fn straight_alpha_over(dst: Color32, src: Color32) -> Color32 {
    if dst.a == 0 || src.a == 255 {
        return src;
    }
    let sa = src.a as f32 / 255.0;
    let da = dst.a as f32 / 255.0 * (1.0 - sa);
    let a = sa + da;
    if a <= 0.0 {
        return Color32::TRANSPARENT;
    }
    let blend = |s: u8, d: u8| ((s as f32 * sa + d as f32 * da) / a).round() as u8;
    Color32::new(
        blend(src.r, dst.r),
        blend(src.g, dst.g),
        blend(src.b, dst.b),
        (a * 255.0).round() as u8,
    )
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
    fill_rect(
        buffer,
        buf_w,
        buf_h,
        0,
        y_offset,
        buf_w as i32,
        lane_height,
        lane_bg,
    );
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
            &mut buf, 320, 56, level, 0, 320, // x range
            0, 56, // y offset, lane height
            0.0, 320.0, // waveform position and width
            0.0, 1.0, // whole-file window
        );

        // Some pixels should be non-transparent (waveform was drawn)
        let non_transparent = buf.iter().filter(|c| c.a > 0).count();
        assert!(
            non_transparent > 0,
            "Waveform should have drawn some pixels"
        );
    }

    fn paint(samples: &[f32], width: usize, height: usize, start: f32, end: f32) -> Vec<Color32> {
        let mut renderer = WaveformRenderer::new();
        renderer.set_audio_data(samples, 1, 48000);
        let level = renderer
            .select_level_for_zoom(width as f32 / (end - start), 1.0)
            .unwrap();
        let mut pixels = vec![Color32::TRANSPARENT; width * height];
        draw_waveform(
            &mut pixels,
            width,
            height,
            level,
            0,
            width as i32,
            0,
            height as i32,
            0.0,
            width as f32,
            start,
            end,
        );
        pixels
    }

    #[test]
    fn waveform_silence_and_filter_ringing_do_not_paint_false_attacks() {
        assert!(
            paint(&[0.0; 4096], 256, 80, 0.0, 1.0)
                .iter()
                .all(|p| p.a == 0)
        );
        let mut samples = vec![0.0; 4096];
        samples[2048] = 0.9;
        let pixels = paint(&samples, 256, 80, 0.0, 1.0);
        let columns: Vec<_> = (0..256)
            .filter(|&x| (0..80).any(|y| pixels[y * 256 + x].a > 0))
            .collect();
        assert_eq!(columns, [128], "only the source impulse bin may be visible");
    }

    #[test]
    fn waveform_pixel_pools_peaks_between_old_point_samples() {
        let mut samples = vec![0.0; 4096];
        samples[128] = 1.0;
        let pixels = paint(&samples, 33, 80, 0.0, 1.0);
        assert!(
            (0..80).any(|y| pixels[y * 33 + 1].a > 0),
            "bin 2 is inside pixel 1 and must not be skipped"
        );
    }

    #[test]
    fn waveform_trim_preserves_source_position() {
        let mut samples = vec![0.0; 4096];
        samples[3072] = 1.0;
        let pixels = paint(&samples, 128, 80, 0.5, 1.0);
        let columns: Vec<_> = (0..128)
            .filter(|&x| (0..80).any(|y| pixels[y * 128 + x].a > 0))
            .collect();
        assert_eq!(columns, [64]);
    }

    #[test]
    fn waveform_bands_have_distinct_colours_and_antialiased_edges() {
        for (frequency, expected) in [
            (60.0, color::WAVEFORM_LOW),
            (800.0, color::WAVEFORM_MID),
            (6000.0, color::WAVEFORM_HIGH),
        ] {
            let samples: Vec<_> = (0..4800)
                .map(|i| (i as f32 * frequency * std::f32::consts::TAU / 48000.0).sin() * 0.67)
                .collect();
            let pixels = paint(&samples, 160, 81, 0.2, 0.8);
            let solid: Vec<_> = pixels.iter().filter(|p| p.a == 255).collect();
            let matching = solid.iter().filter(|&&&p| p == expected).count();
            assert!(
                matching * 2 > solid.len(),
                "{frequency} Hz should predominantly have its band colour"
            );
            assert!(
                pixels.iter().any(|p| p.a > 0 && p.a < 255),
                "fractional edges need coverage alpha"
            );
        }
    }

    #[test]
    fn waveform_straight_alpha_keeps_edge_colour_bright() {
        let src = Color32::new(100, 150, 200, 64);
        assert_eq!(straight_alpha_over(Color32::TRANSPARENT, src), src);
        assert_eq!(
            straight_alpha_over(Color32::new(20, 20, 20, 255), src),
            Color32::new(40, 53, 65, 255)
        );
    }
}
