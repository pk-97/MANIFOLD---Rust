//! Inject a deterministic test-pattern atlas and render thumbnail windows over
//! the clip bodies through the REAL `ClipThumbGpu` path — so clip previews (and,
//! layered onto this same seam, the §F aspect-locked window) render headless
//! without the content thread. See `docs/HEADLESS_UI_HARNESS.md` §5 / Phase 3.
//!
//! Mirrors the app's clip-thumbnail pass: each non-audio clip RESERVES the bottom
//! name-strip band (the thumbnail tiles only the preview area above it) and lays a
//! filmstrip of aspect-locked windows (`win_w = preview_height × cell_aspect`)
//! across the preview, each window sampling a DISTINCT atlas cell — so the headless
//! render shows the real two-band anatomy + a varied filmstrip, not one stretched
//! still.

use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::clip_thumb_gpu::ThumbQuad;
use manifold_ui::panels::viewport::ClipScreenRect;

use crate::content_pipeline::{
    CLIP_ATLAS_CELL_H, CLIP_ATLAS_CELL_W, CLIP_ATLAS_COLS, CLIP_ATLAS_H, CLIP_ATLAS_ROWS,
    CLIP_ATLAS_W,
};

const CELLS: u32 = CLIP_ATLAS_COLS * CLIP_ATLAS_ROWS;

/// Build a test-pattern atlas (every cell a distinct mid-bright gradient + grid,
/// comfortably above the thumbnail shader's luminance gate) and upload it.
pub fn make_test_atlas(device: &GpuDevice) -> GpuTexture {
    let w = CLIP_ATLAS_W as usize;
    let h = CLIP_ATLAS_H as usize;
    let cw = CLIP_ATLAS_CELL_W as usize;
    let ch = CLIP_ATLAS_CELL_H as usize;
    let mut buf = vec![0u8; w * h * 4];

    for cell in 0..CELLS as usize {
        let ox = (cell as u32 % CLIP_ATLAS_COLS) as usize * cw;
        let oy = (cell as u32 / CLIP_ATLAS_COLS) as usize * ch;
        let (tr, tg, tb) = cell_tint(cell);
        for y in 0..ch {
            for x in 0..cw {
                let ramp = ((x + y) * 200 / (cw + ch)) as u8; // diagonal gradient
                let grid = if x % 16 == 0 || y % 16 == 0 { 70u8 } else { 0 };
                let i = ((oy + y) * w + (ox + x)) * 4;
                buf[i] = tr.saturating_add(ramp).saturating_add(grid).max(48);
                buf[i + 1] = tg.saturating_add(ramp).saturating_add(grid).max(48);
                buf[i + 2] = tb.saturating_add(ramp).saturating_add(grid).max(48);
                buf[i + 3] = 255;
            }
        }
    }

    let tex = device.create_texture(&GpuTextureDesc {
        width: CLIP_ATLAS_W,
        height: CLIP_ATLAS_H,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
        label: "ui-snap test atlas",
        mip_levels: 1,
    });
    device.upload_texture(&tex, &buf);
    tex
}

fn cell_tint(cell: usize) -> (u8, u8, u8) {
    (
        ((cell * 53) % 160) as u8,
        ((cell * 97) % 160) as u8,
        ((cell * 151) % 160) as u8,
    )
}

/// A filmstrip of aspect-locked windows per non-audio clip, tiled across the
/// PREVIEW area (the clip minus its reserved name strip), each window a distinct
/// atlas cell. Matches the app's `width < 24` / audio skip filter and the same
/// `clip_strip_height` reservation, so the headless render is faithful to the app.
pub fn build_quads(clip_rects: &[ClipScreenRect]) -> Vec<ThumbQuad> {
    use manifold_ui::node::Rect;
    let inv_cols = 1.0 / CLIP_ATLAS_COLS as f32;
    let inv_rows = 1.0 / CLIP_ATLAS_ROWS as f32;
    let cell_aspect = CLIP_ATLAS_CELL_W as f32 / CLIP_ATLAS_CELL_H as f32;
    let mut quads = Vec::new();
    for (i, cr) in clip_rects.iter().enumerate() {
        if cr.is_audio || cr.rect.width < 24.0 {
            continue;
        }
        // Reserve the bottom name strip, then inset by CLIP_THUMB_INSET so the well
        // frames the thumbnail as a panel — mirrors the app's clip-thumbnail pass.
        let strip_h = manifold_renderer::clip_draw::clip_strip_height(cr.rect.height)
            .unwrap_or(0.0);
        let m = manifold_ui::color::CLIP_THUMB_INSET;
        let preview_h = (cr.rect.height - strip_h).max(1.0);
        let preview = Rect::new(
            cr.rect.x + m,
            cr.rect.y + m,
            (cr.rect.width - 2.0 * m).max(1.0),
            (preview_h - 2.0 * m).max(1.0),
        );
        let win_w = preview.height * cell_aspect;
        if win_w < 1.0 {
            continue;
        }
        let right = preview.x + preview.width;
        let mut x = preview.x;
        let mut k = 0u32;
        while x < right - 0.5 {
            let w = win_w.min(right - x);
            // A distinct cell per window (and per clip) — a varied filmstrip.
            let cell = (i as u32 * 7 + k) % CELLS;
            let gx = (cell % CLIP_ATLAS_COLS) as f32;
            let gy = (cell / CLIP_ATLAS_COLS) as f32;
            let (u0, v0) = (gx * inv_cols, gy * inv_rows);
            quads.push(ThumbQuad {
                rect: Rect::new(x, preview.y, w, preview.height),
                body_rect: preview,
                radius: manifold_ui::color::CLIP_RADIUS,
                uv_min: [u0, v0],
                uv_max: [u0 + inv_cols, v0 + inv_rows],
            });
            x += win_w;
            k += 1;
        }
    }
    quads
}
