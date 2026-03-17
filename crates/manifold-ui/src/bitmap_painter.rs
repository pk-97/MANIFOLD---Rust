//! Static pixel-level drawing primitives for painting clip rectangles into
//! per-layer pixel buffers. Operates on `&mut [Color32]` arrays — no allocations.
//!
//! Mechanical translation of `Assets/Scripts/UI/Timeline/LayerBitmapPainter.cs`.

use crate::color;
use crate::node::Color32;

// ── Color constants (from Unity LayerBitmapPainter lines 15-21) ──

const BORDER_NORMAL: Color32 = color::ACCENT_BLUE;
const BORDER_SELECTED: Color32 = color::SELECTED_BORDER;
const TRIM_HINT_COLOR: Color32 = color::ACCENT_BLUE_DIM;
const CLIP_SEPARATOR: Color32 = color::CLIP_SEPARATOR;

pub const REGION_HIGHLIGHT_COLOR: Color32 = color::ACCENT_BLUE_SELECTION;
pub const INSERT_CURSOR_COLOR: Color32 = color::INSERT_CURSOR_BLUE;

/// Scale a logical pixel value to texture pixels, minimum 1.
/// Unity: `private static int S(int logicalPx, float renderScale)
///   => Mathf.Max(1, Mathf.RoundToInt(logicalPx * renderScale));`
#[inline]
pub fn s(logical_px: i32, render_scale: f32) -> i32 {
    (logical_px as f32 * render_scale).round().max(1.0) as i32
}

/// Standard "over" alpha-blend.
/// Unity integer math with +127 rounding (LayerBitmapPainter lines 122-131):
///   `r = (src.r * sa + dst.r * (255 - sa) + 127) / 255`
#[inline]
pub fn alpha_blend(dst: Color32, src: Color32) -> Color32 {
    let sa = src.a as u32;
    let da = 255 - sa;
    Color32::new(
        ((src.r as u32 * sa + dst.r as u32 * da + 127) / 255) as u8,
        ((src.g as u32 * sa + dst.g as u32 * da + 127) / 255) as u8,
        ((src.b as u32 * sa + dst.b as u32 * da + 127) / 255) as u8,
        (src.a as u32 + dst.a as u32).min(255) as u8,
    )
}

/// Bounds-checked pixel fill with alpha-blend when `color.a < 255`.
/// Unity: LayerBitmapPainter.FillRect (lines 23-41).
pub fn fill_rect(
    buffer: &mut [Color32],
    tex_w: usize,
    tex_h: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: Color32,
) {
    let x0 = x.max(0) as usize;
    let y0 = y.max(0) as usize;
    let x1 = ((x + w) as usize).min(tex_w);
    let y1 = ((y + h) as usize).min(tex_h);

    if x0 >= x1 || y0 >= y1 {
        return;
    }

    let blend = color.a < 255;

    for py in y0..y1 {
        let row_start = py * tex_w;
        for px in x0..x1 {
            buffer[row_start + px] = if blend {
                alpha_blend(buffer[row_start + px], color)
            } else {
                color
            };
        }
    }
}

/// Draw 4-edge border via FillRect calls.
/// Unity: LayerBitmapPainter.DrawBorder (lines 43-50).
pub fn draw_border(
    buffer: &mut [Color32],
    tex_w: usize,
    tex_h: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: Color32,
    thickness: i32,
) {
    // Bottom
    fill_rect(buffer, tex_w, tex_h, x, y + h - thickness, w, thickness, color);
    // Top
    fill_rect(buffer, tex_w, tex_h, x, y, w, thickness, color);
    // Left
    fill_rect(buffer, tex_w, tex_h, x, y, thickness, h, color);
    // Right
    fill_rect(buffer, tex_w, tex_h, x + w - thickness, y, thickness, h, color);
}

/// Draw a single clip rectangle with background, separator, borders, and trim hints.
/// Unity: LayerBitmapPainter.DrawClip (lines 52-92).
///
/// Rendering order:
/// 1. FillRect background
/// 2. 1px dark separator at left edge (if w >= s(4, scale))
/// 3. Top/bottom border always
/// 4. Left/right border only if w >= s(12, scale)
/// 5. Trim hints (s(6, scale) px) if show_trim_hints
pub fn draw_clip(
    buffer: &mut [Color32],
    tex_w: usize,
    tex_h: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bg_color: Color32,
    is_selected: bool,
    show_trim_hints: bool,
    render_scale: f32,
) {
    if w <= 0 || h <= 0 {
        return;
    }

    let border_thickness = if is_selected { s(2, render_scale) } else { s(1, render_scale) };
    let border_color = if is_selected { BORDER_SELECTED } else { BORDER_NORMAL };

    // 1. Background fill
    fill_rect(buffer, tex_w, tex_h, x, y, w, h, bg_color);

    // 2. 1px dark separator at left edge — distinguishes adjacent clips at low zoom
    // (Ableton-style). Only drawn when clip is wide enough that separator is subtle.
    let sep_w = s(1, render_scale);
    if w >= s(4, render_scale) {
        fill_rect(buffer, tex_w, tex_h, x, y, sep_w, h, CLIP_SEPARATOR);
    }

    // 3. Always draw top/bottom borders — consistent horizontal edges,
    // eliminates "caterpillar" ribbing at low zoom.
    fill_rect(buffer, tex_w, tex_h, x, y, w, border_thickness, border_color);
    fill_rect(
        buffer,
        tex_w,
        tex_h,
        x,
        y + h - border_thickness,
        w,
        border_thickness,
        border_color,
    );

    // 4. Left/right borders only on clips wide enough that borders don't dominate.
    // s(12) keeps narrow clips clean at low zoom while showing edges at high zoom.
    if w >= s(12, render_scale) {
        fill_rect(buffer, tex_w, tex_h, x, y, border_thickness, h, border_color);
        fill_rect(
            buffer,
            tex_w,
            tex_h,
            x + w - border_thickness,
            y,
            border_thickness,
            h,
            border_color,
        );

        // 5. Trim hints inside borders
        if show_trim_hints {
            let trim_w = s(6, render_scale).min((w - border_thickness * 2) / 4);
            if trim_w > 0 {
                fill_rect(
                    buffer,
                    tex_w,
                    tex_h,
                    x + border_thickness,
                    y + border_thickness,
                    trim_w,
                    h - border_thickness * 2,
                    TRIM_HINT_COLOR,
                );
                fill_rect(
                    buffer,
                    tex_w,
                    tex_h,
                    x + w - border_thickness - trim_w,
                    y + border_thickness,
                    trim_w,
                    h - border_thickness * 2,
                    TRIM_HINT_COLOR,
                );
            }
        }
    }
}

/// Get the background color for a clip based on its visual state.
/// Unity: LayerBitmapPainter.GetClipColor (lines 94-120).
/// Priority: locked → selected → hovered → normal. Muted post-blend 50% with MUTED_COLOR.
pub fn get_clip_color(
    is_selected: bool,
    is_hovered: bool,
    is_muted: bool,
    is_locked: bool,
    is_generator: bool,
) -> Color32 {
    let base = if is_locked {
        color::CLIP_LOCKED
    } else if is_selected {
        if is_generator {
            color::CLIP_GEN_SELECTED
        } else {
            color::CLIP_SELECTED
        }
    } else if is_hovered {
        if is_generator {
            color::CLIP_GEN_HOVER
        } else {
            color::CLIP_HOVER
        }
    } else if is_generator {
        color::CLIP_GEN_NORMAL
    } else {
        color::CLIP_NORMAL
    };

    // Muted post-process: blend 50% with MutedColor (rust-orange tint)
    if is_muted {
        let m = color::MUTED_COLOR;
        Color32::new(
            ((base.r as u16 + m.r as u16) / 2) as u8,
            ((base.g as u16 + m.g as u16) / 2) as u8,
            ((base.b as u16 + m.b as u16) / 2) as u8,
            base.a,
        )
    } else {
        base
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_blend_fully_opaque_src() {
        let dst = Color32::new(100, 100, 100, 255);
        let src = Color32::new(200, 50, 10, 255);
        let result = alpha_blend(dst, src);
        assert_eq!(result.r, 200);
        assert_eq!(result.g, 50);
        assert_eq!(result.b, 10);
        assert_eq!(result.a, 255);
    }

    #[test]
    fn alpha_blend_half_transparent() {
        let dst = Color32::new(0, 0, 0, 255);
        let src = Color32::new(200, 100, 50, 128);
        let result = alpha_blend(dst, src);
        // sa=128, da=127
        // r = (200*128 + 0*127 + 127) / 255 = 25727/255 = 100
        assert_eq!(result.r, 100);
        assert_eq!(result.g, 50);
        assert_eq!(result.b, 25);
    }

    #[test]
    fn alpha_blend_fully_transparent_src() {
        let dst = Color32::new(100, 100, 100, 255);
        let src = Color32::new(200, 50, 10, 0);
        let result = alpha_blend(dst, src);
        assert_eq!(result.r, 100);
        assert_eq!(result.g, 100);
        assert_eq!(result.b, 100);
    }

    #[test]
    fn fill_rect_basic() {
        let mut buf = vec![Color32::TRANSPARENT; 4 * 4];
        let c = Color32::new(255, 0, 0, 255);
        fill_rect(&mut buf, 4, 4, 1, 1, 2, 2, c);
        // Row 1: pixels 1,2 should be red
        assert_eq!(buf[1 * 4 + 1], c);
        assert_eq!(buf[1 * 4 + 2], c);
        assert_eq!(buf[2 * 4 + 1], c);
        assert_eq!(buf[2 * 4 + 2], c);
        // Corners should still be transparent
        assert_eq!(buf[0], Color32::TRANSPARENT);
        assert_eq!(buf[3 * 4 + 3], Color32::TRANSPARENT);
    }

    #[test]
    fn fill_rect_clamps_to_bounds() {
        let mut buf = vec![Color32::TRANSPARENT; 4 * 4];
        let c = Color32::new(0, 255, 0, 255);
        // Rect extends beyond texture bounds — should not panic
        fill_rect(&mut buf, 4, 4, -1, -1, 6, 6, c);
        // All pixels should be filled
        for p in &buf {
            assert_eq!(*p, c);
        }
    }

    #[test]
    fn fill_rect_alpha_blend_path() {
        let bg = Color32::new(0, 0, 0, 255);
        let mut buf = vec![bg; 2 * 2];
        let overlay = Color32::new(255, 255, 255, 128);
        fill_rect(&mut buf, 2, 2, 0, 0, 2, 2, overlay);
        // Should be blended, not replaced
        assert!(buf[0].r > 0 && buf[0].r < 255);
    }

    #[test]
    fn s_scaling_minimum_one() {
        assert_eq!(s(1, 0.1), 1); // 0.1 rounds to 0, clamped to 1
        assert_eq!(s(1, 1.0), 1);
        assert_eq!(s(1, 2.0), 2);
        assert_eq!(s(6, 2.0), 12);
    }

    #[test]
    fn draw_clip_zero_size_noop() {
        let mut buf = vec![Color32::TRANSPARENT; 4];
        draw_clip(&mut buf, 2, 2, 0, 0, 0, 2, Color32::new(255, 0, 0, 255), false, false, 1.0);
        // Should not have painted anything
        assert_eq!(buf[0], Color32::TRANSPARENT);
    }

    #[test]
    fn draw_clip_narrow_no_side_borders() {
        // Clip width 8 < s(12,1)=12 → no left/right borders, no trim hints
        let mut buf = vec![Color32::TRANSPARENT; 20 * 10];
        let bg = Color32::new(173, 168, 163, 255);
        draw_clip(&mut buf, 20, 10, 2, 1, 8, 8, bg, false, true, 1.0);
        // Should have painted background + separator + top/bottom border
        // but NOT left/right border or trim hints.
        // Separator is at (x=2, y=1..9) but border overwrites row y=1 (top border).
        // Check separator in a non-border row (y=2, border thickness=1).
        assert_eq!(buf[2 * 20 + 2], CLIP_SEPARATOR); // separator below top border
    }

    #[test]
    fn draw_clip_wide_has_side_borders() {
        // Clip width 20 >= s(12,1)=12 → has left/right borders
        let mut buf = vec![Color32::TRANSPARENT; 30 * 10];
        let bg = Color32::new(173, 168, 163, 255);
        draw_clip(&mut buf, 30, 10, 2, 1, 20, 8, bg, true, false, 1.0);
        // Selected border thickness = s(2,1)=2
        // Left border at x=2, should be BORDER_SELECTED
        assert_eq!(buf[1 * 30 + 2], BORDER_SELECTED);
    }

    #[test]
    fn get_clip_color_priority() {
        // Locked takes priority
        let locked = get_clip_color(true, true, false, true, false);
        assert_eq!(locked, color::CLIP_LOCKED);

        // Selected over hovered
        let selected = get_clip_color(true, true, false, false, false);
        assert_eq!(selected, color::CLIP_SELECTED);

        // Generator selected
        let gen_sel = get_clip_color(true, false, false, false, true);
        assert_eq!(gen_sel, color::CLIP_GEN_SELECTED);

        // Normal video
        let normal = get_clip_color(false, false, false, false, false);
        assert_eq!(normal, color::CLIP_NORMAL);

        // Normal generator
        let gen_normal = get_clip_color(false, false, false, false, true);
        assert_eq!(gen_normal, color::CLIP_GEN_NORMAL);
    }

    #[test]
    fn get_clip_color_muted_blend() {
        let muted = get_clip_color(false, false, true, false, false);
        let base = color::CLIP_NORMAL;
        let m = color::MUTED_COLOR;
        assert_eq!(muted.r, ((base.r as u16 + m.r as u16) / 2) as u8);
        assert_eq!(muted.g, ((base.g as u16 + m.g as u16) / 2) as u8);
    }
}
