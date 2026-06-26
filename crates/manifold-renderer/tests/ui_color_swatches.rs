//! Colour contact sheet — renders the §15 semantic ramp and the state colours
//! re-pointed onto it to a PNG, so the ramp can be eyeballed headlessly (no
//! running app). Reuses the same windowless render path as the headless UI
//! spike (docs §23): `GpuDevice::new()` → `UIRenderer` immediate draws →
//! texture readback → PNG.
//!
//! Run: `SWATCH_OUT=/some/dir cargo test -p manifold-renderer --test ui_color_swatches`
//! then open `$SWATCH_OUT/color_ramp.png`.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::color;
use manifold_ui::node::Color32;

// W*4 must be 256-byte aligned for the texture→buffer readback copy.
// 640*4 = 2560 = 10*256. H is unconstrained.
const W: u32 = 660;
const H: u32 = 640;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

const ROW_H: f32 = 24.0;
const SW_X: f32 = 14.0;
const SW_W: f32 = 130.0;
const SW_H: f32 = 18.0;

#[test]
fn color_ramp_contact_sheet() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/color_ramp.png");

    // The 7×3 ramp (one role-hue per group), left column.
    let ramp: &[(&str, Color32)] = &[
        ("RED_IDLE", color::RED_IDLE),
        ("RED_BASE", color::RED_BASE),
        ("RED_ACTIVE", color::RED_ACTIVE),
        ("GREEN_IDLE", color::GREEN_IDLE),
        ("GREEN_BASE", color::GREEN_BASE),
        ("GREEN_ACTIVE", color::GREEN_ACTIVE),
        ("AMBER_IDLE", color::AMBER_IDLE),
        ("AMBER_BASE", color::AMBER_BASE),
        ("AMBER_ACTIVE", color::AMBER_ACTIVE),
        ("ORANGE_IDLE", color::ORANGE_IDLE),
        ("ORANGE_BASE", color::ORANGE_BASE),
        ("ORANGE_ACTIVE", color::ORANGE_ACTIVE),
        ("BLUE_IDLE", color::BLUE_IDLE),
        ("BLUE_BASE", color::BLUE_BASE),
        ("BLUE_ACTIVE", color::BLUE_ACTIVE),
        ("CYAN_IDLE", color::CYAN_IDLE),
        ("CYAN_BASE", color::CYAN_BASE),
        ("CYAN_ACTIVE", color::CYAN_ACTIVE),
        ("PURPLE_IDLE", color::PURPLE_IDLE),
        ("PURPLE_BASE", color::PURPLE_BASE),
        ("PURPLE_ACTIVE", color::PURPLE_ACTIVE),
    ];

    // The state colours now aliased onto the ramp, right column.
    let state: &[(&str, Color32)] = &[
        ("PLAY_GREEN", color::PLAY_GREEN),
        ("PLAY_ACTIVE", color::PLAY_ACTIVE),
        ("STOP_RED", color::STOP_RED),
        ("RECORD_RED", color::RECORD_RED),
        ("RECORD_ACTIVE", color::RECORD_ACTIVE),
        ("PLAYHEAD_RED", color::PLAYHEAD_RED),
        ("EXPORT_ACTIVE", color::EXPORT_ACTIVE),
        ("MONITOR_ACTIVE", color::MONITOR_ACTIVE),
        ("PAUSED_YELLOW", color::PAUSED_YELLOW),
        ("BPM_RESET_ACTIVE", color::BPM_RESET_ACTIVE),
        ("BPM_CLEAR_ACTIVE", color::BPM_CLEAR_ACTIVE),
        ("MUTE_BTN_ACTIVE", color::MUTE_BTN_ACTIVE),
        ("SOLO_BTN_ACTIVE", color::SOLO_BTN_ACTIVE),
        ("STATUS_GOOD", color::STATUS_GOOD),
        ("STATUS_WARNING", color::STATUS_WARNING),
        ("STATUS_BAD", color::STATUS_BAD),
        ("STATUS_ACTIVE", color::STATUS_ACTIVE),
        ("ACCENT_BLUE", color::ACCENT_BLUE),
        ("SELECTED_BORDER", color::SELECTED_BORDER),
    ];

    ui.begin_frame();
    // App-background fill so swatches sit on the real panel grey.
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);

    ui.draw_text(SW_X, 8.0, "SEMANTIC RAMP (§15)", 13.0, color::TEXT_NORMAL);
    draw_column(&mut ui, SW_X, 32.0, ramp);

    let col2 = 350.0;
    ui.draw_text(col2, 8.0, "STATE -> RAMP", 13.0, color::TEXT_NORMAL);
    draw_column(&mut ui, col2, 32.0, state);

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "swatch sheet produced no draw commands");

    let target = RenderTarget::new(&device, W, H, FORMAT, "swatch-sheet");
    {
        let mut enc = device.create_encoder("swatch-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }

    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("colour contact sheet → {png}");
}

/// Renders a floating surface with and without the §17 soft shadow, so the
/// "lift" can be eyeballed headlessly.
#[test]
fn shadow_demo() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/shadow_demo.png");

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);

    // Left — a floating surface WITHOUT a shadow (flat, glued to the panel).
    ui.draw_text(60.0, 40.0, "no shadow", 13.0, color::TEXT_NORMAL);
    ui.draw_bordered_rect(60.0, 70.0, 220.0, 150.0, color::BG_2, 6.0, 1.0, color::BORDER);

    // Right — the same surface WITH the soft drop-shadow under it (lifts off).
    ui.draw_text(380.0, 40.0, "soft shadow (\u{00a7}17)", 13.0, color::TEXT_NORMAL);
    ui.draw_shadow(383.0, 75.0, 220.0, 150.0, 6.0, 16.0, Color32::new(0, 0, 0, 150));
    ui.draw_bordered_rect(380.0, 70.0, 220.0, 150.0, color::BG_2, 6.0, 1.0, color::BORDER);

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "shadow demo produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "shadow-demo");
    {
        let mut enc = device.create_encoder("shadow-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("shadow demo → {png}");
}

fn draw_column(ui: &mut UIRenderer, x: f32, y0: f32, rows: &[(&str, Color32)]) {
    for (i, (label, c)) in rows.iter().enumerate() {
        let y = y0 + i as f32 * ROW_H;
        ui.draw_rect(x, y, SW_W, SW_H, *c);
        ui.draw_text(x + SW_W + 10.0, y + 3.0, label, 12.0, color::TEXT_NORMAL);
    }
}

/// Texture → CPU bytes, same pattern as the headless spike / parity harness.
fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("swatch-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
