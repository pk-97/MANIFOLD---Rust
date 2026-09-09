//! Cell-arc repro — headless pixel proof for the IMAGE_SHADER rounded-rect
//! mask on atlas sub-rect uvs (browser audition cells).
//!
//! The bug: `ui_renderer.rs` IMAGE_SHADER `fs_main` computes the rounded-rect
//! SDF in atlas-uv space (`pixel = in.uv * vec2(rect_w, rect_h)`). For image
//! nodes sampling a sub-rect uv (audition cells), `in.uv` spans only that
//! sub-range, so the SDF rounds the corner matching the cell's ATLAS slot
//! (col 0 → top-left, col 15 → top-right, interior → no corner clips at
//! all) instead of the drawn quad's corners. This test draws cells from
//! several atlas slots with known per-cell gradients and asserts every
//! drawn quad gets symmetric corner masking.
//!
//! Run: `CELL_OUT=/some/dir cargo test -p manifold-renderer --test ui_cell_arc_repro`
//! then open `$CELL_OUT/cell_radius6.png` / `cell_radius0.png`.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{
    GpuDevice, GpuLoadAction, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::{texture_handle_for_key, UIFlags};
use manifold_ui::{Rect, UITree, ZTier};

// W*4 must be 256-byte aligned for the texture→buffer readback copy.
// 1024*4 = 4096 = 16*256. H is unconstrained.
const W: u32 = 1024;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

// Synthetic audition-style atlas: 16 cols × 2 rows of 256×144 cells.
const COLS: u32 = 16;
const ROWS: u32 = 2;
const CELL_W: u32 = 256;
const CELL_H: u32 = 144;
const ATLAS_W: u32 = COLS * CELL_W;
const ATLAS_H: u32 = ROWS * CELL_H;

// Drawn cell size, matching browser_popup.rs CELL_W/CELL_H/CELL_RADIUS.
const DRAW_W: f32 = 170.0;
const DRAW_H: f32 = 96.0;
const DRAW_RADIUS: f32 = 6.0;

/// (col, row) atlas slots drawn, and where each lands on screen.
const CELLS: &[(u32, u32, f32, f32)] = &[
    (0, 0, 20.0, 20.0),
    (3, 0, 200.0, 20.0),
    (7, 0, 380.0, 20.0),
    (12, 0, 560.0, 20.0),
    (15, 0, 740.0, 20.0),
    (0, 1, 200.0, 126.0),
    (15, 1, 380.0, 126.0),
];
const CANVAS_H: u32 = 242;

/// Half-texel-inset uv sub-rect, exactly `audition/mod.rs::rebuild_uvs`.
fn cell_uv(col: u32, row: u32) -> [f32; 4] {
    let gx = col as f32 * CELL_W as f32;
    let gy = row as f32 * CELL_H as f32;
    [
        (gx + 0.5) / ATLAS_W as f32,
        (gy + 0.5) / ATLAS_H as f32,
        (gx + CELL_W as f32 - 0.5) / ATLAS_W as f32,
        (gy + CELL_H as f32 - 0.5) / ATLAS_H as f32,
    ]
}

/// Per-cell horizontal gradient: left = hue(i), right = hue(i + 60°), where
/// `i = row * COLS + col`. A cell's two ends differ strongly from every
/// neighbor's, so sampling across a cell boundary or into the wrong cell
/// shows up as a hard vertical split in the wrong color.
fn build_atlas() -> Vec<u8> {
    fn hue_to_rgb(h: f32) -> (f32, f32, f32) {
        let h = h.rem_euclid(360.0);
        let c = 1.0;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let (r, g, b) = match h {
            h if h < 60.0 => (c, x, 0.0),
            h if h < 120.0 => (x, c, 0.0),
            h if h < 180.0 => (0.0, c, x),
            h if h < 240.0 => (0.0, x, c),
            h if h < 300.0 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        (r * 255.0, g * 255.0, b * 255.0)
    }
    let mut rgba = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];
    for row in 0..ROWS {
        for col in 0..COLS {
            let i = row * COLS + col;
            let (lr, lg, lb) = hue_to_rgb(i as f32 * 137.5);
            let (rr, rg, rb) = hue_to_rgb(i as f32 * 137.5 + 60.0);
            for y in 0..CELL_H {
                for x in 0..CELL_W {
                    let t = x as f32 / (CELL_W - 1) as f32;
                    let px = ((col * CELL_W + x) + (row * CELL_H + y) * ATLAS_W) * 4;
                    rgba[px as usize] = (lr + (rr - lr) * t).round() as u8;
                    rgba[px as usize + 1] = (lg + (rg - lg) * t).round() as u8;
                    rgba[px as usize + 2] = (lb + (rb - lb) * t).round() as u8;
                    rgba[px as usize + 3] = 255;
                }
            }
        }
    }
    rgba
}

fn render_with(
    tree: &UITree,
    device: &GpuDevice,
    ui: &mut UIRenderer,
    h: u32,
) -> Vec<u8> {
    ui.begin_frame();
    ui.render_tree(tree, None);
    assert!(ui.prepare(device, W, h, 1.0), "cell repro produced no draw commands");
    let target = RenderTarget::new(device, W, h, FORMAT, "cell-arc-render");
    {
        let mut enc = device.create_encoder("cell-arc-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    readback(device, &target.texture, h)
}

fn readback(device: &GpuDevice, texture: &GpuTexture, h: u32) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("cell-arc-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}

fn save(out_dir: &str, name: &str, bytes: &[u8], h: u32) {
    image::save_buffer(
        format!("{out_dir}/{name}"),
        bytes,
        W,
        h,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap_or_else(|e| panic!("save {name}: {e}"));
    eprintln!("{name} → {out_dir}/{name}");
}

fn px(bytes: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b.iter()).all(|(&x, &y)| x.abs_diff(y) <= tol)
}

/// Expected gradient color of atlas cell (col, row) at local u in 0..1.
fn expected_cell_color(col: u32, row: u32, u: f32) -> [u8; 4] {
    fn hue_to_rgb(h: f32) -> (f32, f32, f32) {
        let h = h.rem_euclid(360.0);
        let c = 1.0;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let (r, g, b) = match h {
            h if h < 60.0 => (c, x, 0.0),
            h if h < 120.0 => (x, c, 0.0),
            h if h < 180.0 => (0.0, c, x),
            h if h < 240.0 => (0.0, x, c),
            h if h < 300.0 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        (r * 255.0, g * 255.0, b * 255.0)
    }
    let i = row * COLS + col;
    let (lr, lg, lb) = hue_to_rgb(i as f32 * 137.5);
    let (rr, rg, rb) = hue_to_rgb(i as f32 * 137.5 + 60.0);
    [
        (lr + (rr - lr) * u).round() as u8,
        (lg + (rg - lg) * u).round() as u8,
        (lb + (rb - lb) * u).round() as u8,
        255,
    ]
}

#[test]
fn rounded_mask_is_symmetric_for_atlas_sub_rect_cells() {
    let out_dir = std::env::var("CELL_OUT").unwrap_or_else(|_| "/tmp/cell-arc-v2".into());
    std::fs::create_dir_all(&out_dir).unwrap();

    let device = GpuDevice::new();
    let atlas = device.create_texture(&GpuTextureDesc {
        width: ATLAS_W,
        height: ATLAS_H,
        depth: 1,
        format: FORMAT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
        label: "cell-arc-fixture-atlas",
        mip_levels: 1,
    });
    device.upload_texture(&atlas, &build_atlas());
    let handle = texture_handle_for_key("/cell-arc-repro/atlas");

    // Radius 6 and radius 0 renders.
    let mut bytes_r6 = Vec::new();
    let mut bytes_r0 = Vec::new();
    for (radius, slot) in [(DRAW_RADIUS, &mut bytes_r6), (0.0, &mut bytes_r0)] {
        let mut ui = UIRenderer::new(&device, FORMAT);
        ui.register_external_texture(handle, atlas.clone());
        let mut tree = UITree::new();
        // Root-parented nodes must live inside a region (tree.rs D4 assert).
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, W as f32, CANVAS_H as f32),
            ZTier::Base,
            "cell-arc",
            UIFlags::empty(),
        );
        let start = tree.count();
        for &(col, row, x, y) in CELLS {
            tree.add_image_uv(None, x, y, DRAW_W, DRAW_H, radius, handle, cell_uv(col, row));
        }
        tree.end_region(region, start);
        *slot = render_with(&tree, &device, &mut ui, CANVAS_H);
    }
    save(&out_dir, "cell_radius6.png", &bytes_r6, CANVAS_H);
    save(&out_dir, "cell_radius0.png", &bytes_r0, CANVAS_H);

    // Canvas reference: a pixel no cell reaches.
    let blank = px(&bytes_r6, 1000, 230);

    // 1. Center of every cell samples ITS OWN gradient — catches wrong-cell
    //    sampling and cross-boundary splits outright.
    for &(col, row, x, y) in CELLS {
        let got = px(
            &bytes_r6,
            (x + DRAW_W / 2.0) as u32,
            (y + DRAW_H / 2.0) as u32,
        );
        let want = expected_cell_color(col, row, 0.5);
        assert!(
            near(got, want, 4),
            "center of cell ({col},{row}) must sample its own gradient: got {got:?} want {want:?}"
        );
    }

    // 2. Edge midpoints: left edge shows the cell's left color, right edge
    //    its right color — for EVERY atlas slot. Pre-fix, slots with u0 > 0
    //    clip a corner on the wrong side instead; more importantly a slot
    //    whose sub-rect doesn't touch pixel-space edges paints corners that
    //    should be clipped (assertion 3).
    for &(col, row, x, y) in CELLS {
        let mid_y = (y + DRAW_H / 2.0) as u32;
        let left = px(&bytes_r6, (x + 1.0) as u32, mid_y);
        let want_left = expected_cell_color(col, row, 0.0);
        assert!(
            near(left, want_left, 6),
            "left edge of cell ({col},{row}): got {left:?} want {want_left:?}"
        );
        let right = px(&bytes_r6, (x + DRAW_W - 2.0) as u32, mid_y);
        let want_right = expected_cell_color(col, row, 1.0);
        assert!(
            near(right, want_right, 6),
            "right edge of cell ({col},{row}): got {right:?} want {want_right:?} — \
             a neighbor's color here means sampling crossed the cell boundary"
        );
    }

    // 3. THE mask assertion: with radius 6, all four corners of every drawn
    //    quad are clipped — regardless of where the cell sits in the atlas.
    //    Pre-fix this only holds for the atlas slot whose sub-rect happens
    //    to touch that corner in pixel space (col 0 → top-left only,
    //    col 15 → top-right only, interior slots → no clipping at all).
    //    Probes sit ON the corner pixel (inset 0): 1px inside lands in the
    //    ~2px AA band and is legitimately partial.
    let corner_inset = 0u32;
    for &(col, row, x, y) in CELLS {
        let x = x as u32;
        let y = y as u32;
        for (cx, cy, which) in [
            (x + corner_inset, y + corner_inset, "top-left"),
            (x + DRAW_W as u32 - 1 - corner_inset, y + corner_inset, "top-right"),
            (x + corner_inset, y + DRAW_H as u32 - 1 - corner_inset, "bottom-left"),
            (
                x + DRAW_W as u32 - 1 - corner_inset,
                y + DRAW_H as u32 - 1 - corner_inset,
                "bottom-right",
            ),
        ] {
            let got = px(&bytes_r6, cx, cy);
            assert!(
                near(got, blank, 2),
                "radius-6 render: {which} corner of cell ({col},{row}) at ({cx},{cy}) \
                 must be clipped to canvas, got {got:?}"
            );
        }
    }

    // 4. Radius 0 disables the mask: corners paint with the cell's own
    //    corner colors (also proves assertions 1-3 measure the mask, not
    //    some other clip).
    for &(col, row, x, y) in CELLS {
        let got = px(&bytes_r0, x as u32 + 1, y as u32 + 1);
        let want = expected_cell_color(col, row, 1.0 / (DRAW_W - 1.0));
        assert!(
            near(got, want, 6),
            "radius-0 render: top-left corner of cell ({col},{row}) paints: got {got:?} want {want:?}"
        );
    }
}
