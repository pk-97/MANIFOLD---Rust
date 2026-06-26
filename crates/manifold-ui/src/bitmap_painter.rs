//! Static pixel-level drawing primitives for painting clip rectangles into
//! per-layer pixel buffers. Operates on `&mut [Color32]` arrays — no allocations.
//!
//! Mechanical translation of `Assets/Scripts/UI/Timeline/LayerBitmapPainter.cs`.

use crate::color;
use crate::node::Color32;

// ── Color constants ──
// Clip fills/borders/separators/trim-hints moved to the GPU clip pass (§24 5b,
// `manifold_renderer::clip_draw`); their constants left with them. Region
// highlight + insert cursor are still bitmap-painted (front buffer).

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
    fill_rect(
        buffer,
        tex_w,
        tex_h,
        x,
        y + h - thickness,
        w,
        thickness,
        color,
    );
    // Top
    fill_rect(buffer, tex_w, tex_h, x, y, w, thickness, color);
    // Left
    fill_rect(buffer, tex_w, tex_h, x, y, thickness, h, color);
    // Right
    fill_rect(
        buffer,
        tex_w,
        tex_h,
        x + w - thickness,
        y,
        thickness,
        h,
        color,
    );
}


/// Get the background color for a clip based on its visual state.
/// Uses the exact layer color. Selected/hovered lighten, locked dims.
pub fn get_clip_color(
    is_selected: bool,
    is_hovered: bool,
    is_muted: bool,
    is_locked: bool,
    _is_generator: bool,
    layer_color: Color32,
) -> Color32 {
    if is_locked {
        return color::CLIP_LOCKED;
    }

    // Exact layer color with lighten/darken for state (matches layer header)
    let base = if is_selected {
        color::lighten(layer_color, 30)
    } else if is_hovered {
        color::lighten(layer_color, 15)
    } else {
        Color32::new(layer_color.r, layer_color.g, layer_color.b, 255)
    };

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
        assert_eq!(buf[4 + 1], c);
        assert_eq!(buf[4 + 2], c);
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
    fn get_clip_color_locked_ignores_layer_color() {
        let lc = Color32::new(255, 0, 0, 255);
        let locked = get_clip_color(true, true, false, true, false, lc);
        assert_eq!(locked, color::CLIP_LOCKED);
    }

    #[test]
    fn get_clip_color_normal_uses_layer_color() {
        let lc = Color32::new(200, 100, 50, 255);
        let normal = get_clip_color(false, false, false, false, false, lc);
        // Normal = layer color unchanged (brightness 1.0)
        assert_eq!(normal.r, 200);
        assert_eq!(normal.g, 100);
        assert_eq!(normal.b, 50);
    }

    #[test]
    fn get_clip_color_selected_lightens() {
        let lc = Color32::new(100, 100, 100, 255);
        let selected = get_clip_color(true, false, false, false, false, lc);
        // Selected = +30 lighten
        assert_eq!(selected.r, 130);
        assert_eq!(selected.g, 130);
    }

    #[test]
    fn get_clip_color_generator_uses_exact_layer_color() {
        let lc = Color32::new(200, 100, 50, 255);
        let gen_normal = get_clip_color(false, false, false, false, true, lc);
        // Generator clips use exact layer color, no tinting
        assert_eq!(gen_normal.r, 200);
        assert_eq!(gen_normal.g, 100);
        assert_eq!(gen_normal.b, 50);
    }

    #[test]
    fn get_clip_color_muted_blend() {
        let lc = Color32::new(200, 100, 50, 255);
        let muted = get_clip_color(false, false, true, false, false, lc);
        let m = color::MUTED_COLOR;
        // Muted = 50% blend of base (layer color) with MUTED_COLOR
        assert_eq!(muted.r, ((200u16 + m.r as u16) / 2) as u8);
        assert_eq!(muted.g, ((100u16 + m.g as u16) / 2) as u8);
    }
}
