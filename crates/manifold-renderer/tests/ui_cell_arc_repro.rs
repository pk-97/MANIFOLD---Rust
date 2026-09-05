//! Cell-arc repro — minimal headless reproduction of the preset browser's
//! audition-grid image rendering (crates/manifold-ui/src/panels/browser_popup.rs
//! `add_image_uv` cells sampling the audition atlas).
//!
//! Renders a grid of `UINodeType::Image` nodes EXACTLY the way the browser
//! builds them — same cell size (170x96), same `CELL_RADIUS` (6.0), same
//! half-texel-inset uv sub-rects as `AuditionPool::rebuild_uvs`
//! (crates/manifold-renderer/src/audition/mod.rs) — against a synthetic
//! 16-column x 2-row atlas whose cells carry a per-cell tint plus an
//! x/y gradient and a dark border, so mis-sampling, bleed, and mask-space
//! errors are all visible in the output PNG.
//!
//! Two artifacts:
//! - `cells_radius6.png`  — the production configuration (radius 6.0)
//! - `cells_radius0.png`  — control: no rounded-corner mask
//!
//! Run: `SWATCH_OUT=/tmp/cell-arc cargo test -p manifold-renderer --test ui_cell_arc_repro`
//! (defaults to /tmp/cell-arc when SWATCH_OUT is unset).

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::texture_handle_for_key;
use manifold_ui::{Rect, UIFlags, UITree, ZTier};

// Target 1280x768: 1280*4 = 5120 = 20*256 (readback row alignment).
const W: u32 = 1280;
const H: u32 = 768;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

// Browser cell geometry — browser_popup.rs CELL_W/CELL_H/CELL_RADIUS.
const CELL_W: f32 = 170.0;
const CELL_H: f32 = 96.0;
const CELL_RADIUS: f32 = 6.0;
const CELL_PITCH: f32 = CELL_W + 6.0;

// Audition atlas geometry — audition/mod.rs GRID_COLS/CELL_W/CELL_H.
const ATLAS_COLS: u32 = 16;
const ATLAS_ROWS: u32 = 2;
const A_CELL_W: u32 = 256;
const A_CELL_H: u32 = 144;
const ATLAS_W: u32 = ATLAS_COLS * A_CELL_W;
const ATLAS_H: u32 = ATLAS_ROWS * A_CELL_H;

/// uv sub-rect for one atlas slot, mirroring `AuditionPool::rebuild_uvs`:
/// half-texel inset on every edge.
fn cell_uv(col: u32, row: u32) -> [f32; 4] {
    let gx = col as f32 * A_CELL_W as f32;
    let gy = row as f32 * A_CELL_H as f32;
    let aw = ATLAS_W as f32;
    let ah = ATLAS_H as f32;
    [
        (gx + 0.5) / aw,
        (gy + 0.5) / ah,
        (gx + A_CELL_W as f32 - 0.5) / aw,
        (gy + A_CELL_H as f32 - 0.5) / ah,
    ]
}

/// Synthetic atlas: per-cell tint + x/y gradient + 2px dark border + a
/// white L-tick at the cell's inner top-left, so orientation flips and
/// neighbour bleed read unambiguously.
fn build_atlas() -> Vec<u8> {
    let mut rgba = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];
    for row in 0..ATLAS_ROWS {
        for col in 0..ATLAS_COLS {
            // Distinct, deterministic per-cell tint.
            let i = row * ATLAS_COLS + col;
            let (tr, tg, tb) = {
                let h = i.wrapping_mul(2654435761) >> 8;
                ((h & 0xff) as u8, ((h >> 8) & 0xff) as u8, 160u8)
            };
            for ly in 0..A_CELL_H {
                for lx in 0..A_CELL_W {
                    let x = col * A_CELL_W + lx;
                    let y = row * A_CELL_H + ly;
                    let o = ((y * ATLAS_W + x) * 4) as usize;
                    let border = lx < 2 || ly < 2 || lx >= A_CELL_W - 2 || ly >= A_CELL_H - 2;
                    let tick = (4..10).contains(&lx) && (4..6).contains(&ly)
                        || (4..6).contains(&lx) && (4..10).contains(&ly);
                    let (r, g, b): (u8, u8, u8) = if border {
                        (10, 10, 10)
                    } else if tick {
                        (255, 255, 255)
                    } else {
                        // Gradient inside the tint: wrong-region sampling
                        // shifts the ramp visibly.
                        (
                            (u32::from(tr) / 2 + u32::from(tr) * lx / A_CELL_W) as u8,
                            (u32::from(tg) / 2 + u32::from(tg) * ly / A_CELL_H) as u8,
                            tb,
                        )
                    };
                    rgba[o] = r;
                    rgba[o + 1] = g;
                    rgba[o + 2] = b;
                    rgba[o + 3] = 255;
                }
            }
        }
    }
    rgba
}

/// Which atlas slots get drawn, and where on screen. Covers left-edge
/// (col 0), right-edge (col 15), interior, and bottom-row slots — the
/// corners that mis-mask under the atlas-uv-space SDF differ per slot.
const SLOTS: &[(u32, u32)] = &[
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 0),
    (12, 0),
    (13, 0),
    (14, 0),
    (15, 0),
    (0, 1),
    (15, 1),
];

fn screen_pos(slot_index: usize, grid_top: f32) -> (f32, f32) {
    // Two rows of cells per grid, mirroring the browser's row pitch.
    let per_row = 7usize;
    let r = slot_index / per_row;
    let c = (slot_index % per_row) as f32;
    (24.0 + c * CELL_PITCH, grid_top + r as f32 * (CELL_H + 10.0))
}

fn render_grid(radius: f32) -> Vec<u8> {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    let handle = texture_handle_for_key("__cell_arc_repro_atlas__");
    let atlas = build_atlas();
    assert!(ui.register_image(&device, handle, ATLAS_W, ATLAS_H, &atlas));

    let mut tree = UITree::new();
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Overlay,
        "cell_arc_repro",
        UIFlags::empty(),
    );
    let start = tree.count();
    for (i, &(col, row)) in SLOTS.iter().enumerate() {
        let (x, y) = screen_pos(i, 24.0);
        tree.add_image_uv(None, x, y, CELL_W, CELL_H, radius, handle, cell_uv(col, row));
    }
    tree.end_region(region, start);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, manifold_ui::color::BG_3);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "cell-arc repro produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "cell-arc-repro");
    {
        let mut enc = device.create_encoder("cell-arc-repro-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    readback(&device, &target.texture)
}

/// Width-parameterized readback; `width * 4` must be 256-byte aligned.
fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("cell-arc-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}

fn save(png: &str, bytes: &[u8]) {
    if let Some(dir) = std::path::Path::new(png).parent() {
        std::fs::create_dir_all(dir).expect("create output dir");
    }
    image::save_buffer(png, bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("cell-arc repro → {png}");
}

/// Production configuration: CELL_RADIUS = 6.0.
#[test]
fn cell_arc_radius6() {
    let out_dir = std::env::var("SWATCH_OUT").unwrap_or_else(|_| "/tmp/cell-arc".into());
    let bytes = render_grid(CELL_RADIUS);
    save(&format!("{out_dir}/cells_radius6.png"), &bytes);
}

/// Control: no rounded-corner mask — isolates the mask as the variable.
#[test]
fn cell_arc_radius0_control() {
    let out_dir = std::env::var("SWATCH_OUT").unwrap_or_else(|_| "/tmp/cell-arc".into());
    let bytes = render_grid(0.0);
    save(&format!("{out_dir}/cells_radius0.png"), &bytes);
}
