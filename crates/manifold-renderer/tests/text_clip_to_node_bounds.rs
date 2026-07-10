//! Regression proof for the node-text containment invariant: a `UINode`'s
//! glyphs are clipped to the node's own rect at enqueue (`draw_node` in
//! `ui_renderer.rs`), so a label longer than its widget cuts at the edge
//! instead of painting over the neighbour. Before this invariant, chrome
//! buttons with long labels (the audio-panel Feature row: "Amplitude",
//! "Transients", …) overran onto adjacent buttons.
//!
//! Same windowless render path as `ui_color_swatches.rs`: `GpuDevice::new()`
//! → `render_tree` → readback → pixel assertions. Also writes a PNG for
//! eyeballing (`SWATCH_OUT=/some/dir` to choose where).

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::{Color32, TextAlign, UIStyle};
use manifold_ui::{Rect, UIFlags, UITree, ZTier};

// W*4 must be 256-byte aligned for the texture→buffer readback copy.
const W: u32 = 640;
const H: u32 = 128;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

const BG: Color32 = Color32::new(10, 10, 10, 255);

// The button under test: deliberately far too narrow for its label.
const BTN_X: f32 = 240.0;
const BTN_Y: f32 = 48.0;
const BTN_W: f32 = 70.0;
const BTN_H: f32 = 28.0;

#[test]
fn overlong_label_stays_inside_button() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    let style = UIStyle {
        // Transparent bg so ANY non-background pixel outside the rect is a
        // leaked glyph, not button fill.
        text_color: Color32::new(255, 255, 255, 255),
        font_size: 13,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    };

    let mut tree = UITree::new();
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Chrome,
        "text-clip-proof",
        UIFlags::empty(),
    );
    let start = tree.count();
    tree.add_button(
        None,
        BTN_X,
        BTN_Y,
        BTN_W,
        BTN_H,
        style,
        "Transients Everywhere Forever",
    );
    tree.end_region(region, start);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, BG);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "fixture produced no draw commands");

    let target = RenderTarget::new(&device, W, H, FORMAT, "text-clip-proof");
    {
        let mut enc = device.create_encoder("text-clip-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);

    // Eyeball copy.
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/text_clip_proof.png");
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("text clip proof → {png}");

    // The renderer converts sRGB → linear before the GPU write, so the
    // readback bytes aren't `BG`'s literal values — sample the actual
    // background from the top-left corner (nothing draws there).
    let bg = [bytes[0], bytes[1], bytes[2]];
    let is_bg = |x: u32, y: u32| -> bool {
        let i = ((y * W + x) * 4) as usize;
        bytes[i] == bg[0] && bytes[i + 1] == bg[1] && bytes[i + 2] == bg[2]
    };

    // 1. Not vacuous: the label did draw inside the button rect.
    let mut inside = 0u32;
    for y in BTN_Y as u32..(BTN_Y + BTN_H) as u32 {
        for x in BTN_X as u32..(BTN_X + BTN_W) as u32 {
            if !is_bg(x, y) {
                inside += 1;
            }
        }
    }
    assert!(inside > 20, "label never drew inside the button ({inside} px)");

    // 2. The invariant: nothing painted outside the button rect — and
    //    overrunning text also respects the horizontal comfort pad
    //    (`TEXT_CLIP_PAD_X` in ui_renderer.rs), so the cut sits a couple of
    //    pixels inside the edge instead of flush against it. Scan the whole
    //    canvas minus the padded rect (+1px guard band for the snapped
    //    quad's far edge landing on the boundary pixel).
    const PAD_X: f32 = 3.0; // mirror of ui_renderer's TEXT_CLIP_PAD_X
    let (x0, y0) = ((BTN_X + PAD_X) as u32 - 1, BTN_Y as u32 - 1);
    let (x1, y1) = (
        (BTN_X + BTN_W - PAD_X) as u32 + 1,
        (BTN_Y + BTN_H) as u32 + 1,
    );
    let mut leaked = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let outside = x < x0 || x >= x1 || y < y0 || y >= y1;
            if outside && !is_bg(x, y) {
                leaked.push((x, y));
            }
        }
    }
    assert!(
        leaked.is_empty(),
        "{} glyph pixels escaped the button rect, first at {:?} — see {png}",
        leaked.len(),
        leaked.first().unwrap()
    );
}

/// Regression proof for the 2026-07-10 pad regression: the comfort pad
/// (`TEXT_CLIP_PAD_X`) must only fire on a side the text actually overruns.
/// Layout sizes label nodes to their measured text, so a well-fitted label
/// legitimately sits flush at the node edge — left-aligned menu rows lost
/// their first glyph ("mport MIDI File"), right-aligned param labels their
/// last, when the pad was applied unconditionally.
#[test]
fn flush_fit_label_keeps_edge_glyphs() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    let base = UIStyle {
        text_color: Color32::new(255, 255, 255, 255),
        font_size: 13,
        text_inset_x: 0.0,
        ..UIStyle::default()
    };

    // Node width = measured text width exactly: flush at both edges, the
    // shape layout produces for every sized-to-text label.
    const LEFT_TEXT: &str = "MIDI"; // 'M': full-height ink at the left edge
    const RIGHT_TEXT: &str = "CHANNEL"; // 'L': ink to the right edge
    let left_w = ui.measure_text_cached(LEFT_TEXT, 13, base.font_weight).x;
    let right_w = ui.measure_text_cached(RIGHT_TEXT, 13, base.font_weight).x;

    let (lx, ly, lh) = (40.0, 20.0, 20.0);
    let (rx, ry, rh) = (40.0, 70.0, 20.0);

    let mut tree = UITree::new();
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Chrome,
        "flush-fit-proof",
        UIFlags::empty(),
    );
    let start = tree.count();
    tree.add_button(
        None,
        lx,
        ly,
        left_w,
        lh,
        UIStyle {
            text_align: TextAlign::Left,
            ..base
        },
        LEFT_TEXT,
    );
    tree.add_button(
        None,
        rx,
        ry,
        right_w,
        rh,
        UIStyle {
            text_align: TextAlign::Right,
            ..base
        },
        RIGHT_TEXT,
    );
    tree.end_region(region, start);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, BG);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "fixture produced no draw commands");

    let target = RenderTarget::new(&device, W, H, FORMAT, "flush-fit-proof");
    {
        let mut enc = device.create_encoder("flush-fit-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);

    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/text_clip_flush_fit.png");
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("flush fit proof → {png}");

    let bg = [bytes[0], bytes[1], bytes[2]];
    let ink_in_band = |x0: u32, x1: u32, y0: u32, y1: u32| -> u32 {
        let mut n = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * W + x) * 4) as usize;
                if bytes[i] != bg[0] || bytes[i + 1] != bg[1] || bytes[i + 2] != bg[2] {
                    n += 1;
                }
            }
        }
        n
    };

    // The first 3px of the left-aligned label ('M's left stem) and the last
    // 3px of the right-aligned label ('L's foot) must carry ink — the
    // unconditional pad clipped exactly these bands.
    const PAD_X: u32 = 3; // mirror of ui_renderer's TEXT_CLIP_PAD_X
    let left_band = ink_in_band(lx as u32, lx as u32 + PAD_X, ly as u32, (ly + lh) as u32);
    assert!(
        left_band > 0,
        "left-aligned flush label lost its first glyph edge — see {png}"
    );
    let r_max = (rx + right_w) as u32;
    let right_band = ink_in_band(r_max - PAD_X, r_max + 1, ry as u32, (ry + rh) as u32);
    assert!(
        right_band > 0,
        "right-aligned flush label lost its last glyph edge — see {png}"
    );
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("text-clip-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
