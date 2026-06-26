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

/// Renders the footer chrome bar after the §18 kit migration, so the new
/// neutral/segment button look (grey-raised active vs the old blue) can be seen.
#[test]
fn footer_demo() {
    use manifold_ui::layout::ScreenLayout;
    use manifold_ui::panels::Panel;
    use manifold_ui::panels::footer::FooterPanel;
    use manifold_ui::UITree;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/footer_demo.png");

    let mut footer = FooterPanel::new();
    footer.set_selection_info("Layers: 5   |   Clips: 5");
    footer.set_render_scale(0.75); // 75% is the active scale segment
    let layout = ScreenLayout::new(W as f32, H as f32);
    let mut tree = UITree::new();
    footer.build(&mut tree, &layout);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_0);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "footer produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "footer-demo");
    {
        let mut enc = device.create_encoder("footer-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("footer demo → {png}");
}

/// Renders the transport bar after the §18 `state_button` migration, so the
/// neutral-chip lift (the always-grey NEW/OPEN/SAVE buttons moved from the old
/// 59-grey to the shared `BUTTON_DIM` 71-chip) and the unchanged semantic
/// PLAY=green / STOP=red / REC=red buttons can be eyeballed in situ.
#[test]
fn transport_demo() {
    use manifold_ui::layout::ScreenLayout;
    use manifold_ui::panels::Panel;
    use manifold_ui::panels::transport::TransportPanel;
    use manifold_ui::UITree;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/transport_demo.png");

    let mut transport = TransportPanel::new();
    transport.set_record_state(true, true); // REC armed → red
    // 1920-wide layout so all three groups land; we crop the render to W later
    // is unnecessary — just render the left+center which fit in W.
    let layout = ScreenLayout::new(1920.0, H as f32);
    let mut tree = UITree::new();
    transport.build(&mut tree, &layout);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, 1920.0, H as f32, color::BG_0);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, 1920, H, 1.0);
    assert!(drew, "transport produced no draw commands");
    let target = RenderTarget::new(&device, 1920, H, FORMAT, "transport-demo");
    {
        let mut enc = device.create_encoder("transport-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback_w(&device, &target.texture, 1920, H);
    image::save_buffer(&png, &bytes, 1920, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("transport demo → {png}");
}

/// Renders the header bar after the §18 migration, so the unified neutral chip
/// (zoom −/+ and Audio/Perform/Monitor now share transport's BUTTON_DIM chip —
/// the action buttons moved off the old 59-grey) can be eyeballed in situ.
#[test]
fn header_demo() {
    use manifold_ui::layout::ScreenLayout;
    use manifold_ui::panels::Panel;
    use manifold_ui::panels::header::HeaderPanel;
    use manifold_ui::UITree;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/header_demo.png");

    let mut header = HeaderPanel::new();
    let layout = ScreenLayout::new(1920.0, H as f32);
    let mut tree = UITree::new();
    header.build(&mut tree, &layout);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, 1920.0, H as f32, color::BG_0);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, 1920, H, 1.0);
    assert!(drew, "header produced no draw commands");
    let target = RenderTarget::new(&device, 1920, H, FORMAT, "header-demo");
    {
        let mut enc = device.create_encoder("header-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback_w(&device, &target.texture, 1920, H);
    image::save_buffer(&png, &bytes, 1920, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("header demo → {png}");
}

/// Renders the §18 `state_button` kit output directly: each hue (the M/S/L/A
/// identity quartet + the transport ramp aliases) in OFF (neutral chip) and ON
/// (filled) state, sitting on a layer-colour strip and on the dark bar, so the
/// shared mechanic is visible without standing up a full panel.
#[test]
fn state_button_sheet() {
    use manifold_ui::chrome::components::state_button_style;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/state_button_sheet.png");

    let hues: &[(&str, Color32)] = &[
        ("MUTE (M)", color::MUTED_COLOR),
        ("SOLO (S)", color::SOLO_COLOR),
        ("LED  (L)", color::LED_COLOR),
        ("ANLY (A)", color::ANALYSIS_COLOR),
        ("PLAY", color::PLAY_GREEN),
        ("STOP", color::STOP_RED),
        ("REC", color::RECORD_ACTIVE),
        ("LINK", color::LINK_ORANGE),
    ];

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);
    ui.draw_text(14.0, 8.0, "STATE BUTTON (\u{00a7}18)  off-chip | on-fill", 13.0, color::TEXT_NORMAL);
    // A colour strip behind the right column, to check off-chips read on a card.
    let strip_x = 360.0;
    ui.draw_rect(strip_x - 10.0, 28.0, 290.0, hues.len() as f32 * 30.0 + 8.0, Color32::new(80, 70, 110, 255));

    for (i, (label, hue)) in hues.iter().enumerate() {
        let y = 34.0 + i as f32 * 30.0;
        ui.draw_text(14.0, y + 4.0, label, 12.0, color::TEXT_NORMAL);
        // On the dark bar (left).
        let off = state_button_style(*hue, false);
        let on = state_button_style(*hue, true);
        ui.draw_rect(120.0, y, 50.0, 22.0, off.bg_color);
        ui.draw_rect(180.0, y, 50.0, 22.0, on.bg_color);
        // On the colour strip (right) — same styles, busier background.
        ui.draw_rect(strip_x, y, 50.0, 22.0, off.bg_color);
        ui.draw_rect(strip_x + 60.0, y, 50.0, 22.0, on.bg_color);
        ui.draw_rect(strip_x + 130.0, y, 50.0, 22.0, on.hover_bg_color);
        ui.draw_rect(strip_x + 190.0, y, 50.0, 22.0, on.pressed_bg_color);
    }
    ui.draw_text(120.0, 34.0 + hues.len() as f32 * 30.0 + 6.0, "cols: off  on  |  off  on  hover  press", 11.0, color::TEXT_DIMMED_C32);

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "state button sheet produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "state-button-sheet");
    {
        let mut enc = device.create_encoder("state-button-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("state button sheet → {png}");
}

/// Renders the browser popup after the §18 shared-shell migration, so the modal
/// container (now ONE rounded 1px-bordered panel via `popup_shell`, replacing the
/// old outer+inner fake-border pair) can be eyeballed. The §17 drop-shadow is
/// app-composited, so it does not appear here — this checks the container + cells.
#[test]
fn browser_popup_demo() {
    use manifold_ui::node::Vec2;
    use manifold_ui::panels::browser_popup::{
        BrowserPopupMode, BrowserPopupPanel, BrowserPopupRequest,
    };
    use manifold_ui::panels::InspectorTab;
    use manifold_ui::UITree;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/browser_popup_demo.png");

    let names: Vec<String> = ["Blur", "Glitch", "Edge Stretch", "Feedback", "Chroma", "Pixelate"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let cats: Vec<String> = names.iter().map(|_| "Stylize".to_string()).collect();
    let type_ids: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();

    let mut popup = BrowserPopupPanel::new();
    popup.set_screen_size(W as f32, H as f32);
    popup.open(BrowserPopupRequest {
        mode: BrowserPopupMode::Effect,
        tab: InspectorTab::Layer,
        layer_id: None,
        item_names: names,
        item_categories: cats,
        category_names: vec!["Stylize".to_string()],
        item_type_ids: type_ids,
        item_search: None,
        spawn_graph_pos: None,
        paste_count: 0,
        screen_anchor: Vec2::new(30.0, 40.0),
    });

    let mut tree = UITree::new();
    popup.build(&mut tree);

    ui.begin_frame();
    // A lighter fill so the dark modal + its 1px border stand out (the popup's
    // own scrim then dims this, as in the app).
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_3);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "browser popup produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "browser-popup-demo");
    {
        let mut enc = device.create_encoder("browser-popup-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("browser popup demo → {png}");
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

/// Renders the §24 5a gradient primitive: a flat control rect, then vertical /
/// horizontal / rounded gradient rects, so the new `draw_gradient_rect` and the
/// shared-shader change can be eyeballed (and flat rects confirmed unregressed).
#[test]
fn gradient_demo() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/gradient_demo.png");

    let top = Color32::new(90, 130, 210, 255);
    let bot = Color32::new(30, 44, 80, 255);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);

    // Flat control (no gradient) — must look exactly as before the vertex change.
    ui.draw_text(40.0, 30.0, "flat", 13.0, color::TEXT_NORMAL);
    ui.draw_rect(40.0, 54.0, 160.0, 90.0, top);

    // Vertical gradient top→bottom.
    ui.draw_text(230.0, 30.0, "vertical", 13.0, color::TEXT_NORMAL);
    ui.draw_gradient_rect(230.0, 54.0, 160.0, 90.0, 0.0, top, bot, [0.0, 1.0]);

    // Horizontal gradient left→right.
    ui.draw_text(420.0, 30.0, "horizontal", 13.0, color::TEXT_NORMAL);
    ui.draw_gradient_rect(420.0, 54.0, 160.0, 90.0, 0.0, top, bot, [1.0, 0.0]);

    // Rounded vertical gradient (clip/card body look).
    ui.draw_text(40.0, 180.0, "rounded + vertical (clip body)", 13.0, color::TEXT_NORMAL);
    ui.draw_gradient_rect(40.0, 204.0, 360.0, 70.0, color::CARD_RADIUS, top, bot, [0.0, 1.0]);

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "gradient demo produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "gradient-demo");
    {
        let mut enc = device.create_encoder("gradient-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("gradient demo → {png}");
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
    readback_w(device, texture, W, H)
}

/// Width-parameterized readback (the transport demo renders at 1920 wide).
/// `width * 4` must be 256-byte aligned (1920*4 = 7680 = 30*256).
fn readback_w(device: &GpuDevice, texture: &GpuTexture, width: u32, height: u32) -> Vec<u8> {
    let bytes_per_row = width * 4;
    let total = u64::from(height * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("swatch-readback");
    enc.copy_texture_to_buffer(texture, &buf, width, height, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
