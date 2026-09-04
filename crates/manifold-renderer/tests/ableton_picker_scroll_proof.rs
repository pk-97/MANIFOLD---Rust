//! Ableton picker scroll proof — renders the macro picker with a 30-track
//! session (content far taller than the height-capped popup) headlessly and
//! asserts at the pixel level that rows never paint outside the container and
//! that scrolling reaches the last track.
//!
//! Regression for BUG-cwkv (ableton-picker-scroll-for-clipped-lists): the
//! picker clamped its height but laid out every row, so a long track/macro
//! list was clipped away with no way to reach it.
//!
//! Run: `SWATCH_OUT=/some/dir cargo test -p manifold-renderer --test ableton_picker_scroll_proof`
//! then open `$SWATCH_OUT/ableton_picker_top.png` / `ableton_picker_bottom.png`.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::{UIFlags, Vec2};
use manifold_ui::panels::ableton_picker::{
    AbletonPickerPopup, AbletonPickerSession, PickerDevice, PickerMacro, PickerTrack,
};
use manifold_ui::panels::overlay::{Overlay, OverlayPlacement};
use manifold_ui::{Rect, UITree, ZTier};

// W*4 must be 256-byte aligned for the texture→buffer readback copy.
const W: u32 = 640;
const H: u32 = 640;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

fn session(n_tracks: usize) -> AbletonPickerSession {
    let mut tracks = Vec::new();
    for i in 0..n_tracks {
        let devices = if i == 0 {
            vec![PickerDevice {
                device_id: 1,
                device_name: "Auto Filter".to_string(),
                device_class_name: "AutoFilter".to_string(),
                macros: vec![
                    PickerMacro { param_id: 1, name: "Filter Cut".to_string() },
                    PickerMacro { param_id: 2, name: "Resonance".to_string() },
                    PickerMacro { param_id: 3, name: "Drive".to_string() },
                ],
            }]
        } else {
            vec![]
        };
        tracks.push(PickerTrack {
            track_id: i as i32,
            track_name: format!("Track {i:02}"),
            devices,
        });
    }
    AbletonPickerSession { rack_tracks: tracks }
}

/// Build the open picker through the overlay-driver path on a fresh tree.
fn build_fresh(dd: &mut AbletonPickerPopup) -> UITree {
    let mut tree = UITree::new();
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Overlay,
        "overlay",
        UIFlags::empty(),
    );
    let start = tree.count();
    Overlay::build_at(
        dd,
        &mut tree,
        OverlayPlacement {
            rect: Rect::ZERO,
            screen: Vec2::new(W as f32, H as f32),
        },
    );
    tree.end_region(region, start);
    tree
}

fn render(tree: &UITree) -> (Vec<u8>, RenderTarget) {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    ui.begin_frame();
    ui.render_tree(tree, None);
    assert!(ui.prepare(&device, W, H, 1.0), "picker produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "picker-scroll");
    {
        let mut enc = device.create_encoder("picker-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    (readback(&device, &target.texture), target)
}

#[test]
fn clipped_list_scrolls_from_top_to_bottom() {
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());

    let mut dd = AbletonPickerPopup::new();
    dd.open(session(30), Vec2::new(60.0, 80.0));
    assert!(dd.is_open());

    // ── State 1: freshly opened, scroll 0 ─────────────────────────
    let tree = build_fresh(&mut dd);
    let (bytes, _t) = render(&tree);
    image::save_buffer(
        format!("{out_dir}/ableton_picker_top.png"),
        &bytes,
        W,
        H,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    // The container bottom is at popup_y + popup_h = 80 + 480 = 560 (height
    // capped). Nothing may paint below it in the track column's x-range.
    let blank = px(600, 600);
    for probe_y in [570u32, 600] {
        for probe_x in [80u32, 160, 240] {
            assert_eq!(
                px(probe_x, probe_y),
                blank,
                "pixel ({probe_x}, {probe_y}) below the container must be untouched"
            );
        }
    }
    // Track 0 is selected → right column shows the Auto Filter macros.
    assert_ne!(px(360, 220), blank, "right column macro visible at the top");

    // ── State 2: scrolled to the bottom ───────────────────────────
    let scroll = manifold_ui::input::UIEvent::Scroll {
        pos: Vec2::new(100.0, 100.0),
        delta: Vec2::new(0.0, -10_000.0),
        modifiers: manifold_ui::input::Modifiers::default(),
    };
    let mut scratch = build_fresh(&mut dd);
    assert!(matches!(
        dd.on_event(&scroll, &mut scratch),
        manifold_ui::panels::overlay::OverlayResponse::Consumed(_)
    ));
    let tree = build_fresh(&mut dd);
    let (bytes, _t) = render(&tree);
    image::save_buffer(
        format!("{out_dir}/ableton_picker_bottom.png"),
        &bytes,
        W,
        H,
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();

    let px = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    };
    let blank = px(600, 600);
    for probe_y in [570u32, 600] {
        for probe_x in [80u32, 160, 240] {
            assert_eq!(
                px(probe_x, probe_y),
                blank,
                "scrolled: pixel ({probe_x}, {probe_y}) below the container untouched"
            );
        }
    }
    // The last track ("Track 29") row paints inside the container: row 29 sits
    // at the bottom of the viewport. Its row fill is TRACK_NORMAL (36,36,38)
    // — assert only "not blank canvas" (unorm round-trip wobble).
    let mut found_last_row = false;
    for y in 500..=558u32 {
        if px(120, y) != blank {
            found_last_row = true;
            break;
        }
    }
    assert!(found_last_row, "scrolled to the bottom: last track row paints");
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("picker-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
