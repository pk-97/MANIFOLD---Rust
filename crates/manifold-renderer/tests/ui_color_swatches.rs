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
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/footer_demo.png");

    let mut footer = FooterPanel::new();
    footer.set_selection_info("Layers: 5   |   Clips: 5");
    let layout = ScreenLayout::new(W as f32, H as f32);
    let mut tree = UITree::new();
    // D4: root-parented panel nodes must be built inside a region bracket
    // (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D4). Full-canvas rect so the
    // region clip is a no-op — the same "clip never crops" idiom the
    // split-handles region and the ui-snapshot single-region wrap use.
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Chrome,
        "footer",
        UIFlags::empty(),
    );
    let start = tree.count();
    footer.build(&mut tree, &layout);
    tree.end_region(region, start);

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
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

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
    // D4: build under a region (see footer_demo). Full-canvas rect → no-op clip.
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, 1920.0, H as f32),
        ZTier::Chrome,
        "transport",
        UIFlags::empty(),
    );
    let start = tree.count();
    transport.build(&mut tree, &layout);
    tree.end_region(region, start);

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
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/header_demo.png");

    let mut header = HeaderPanel::new();
    let layout = ScreenLayout::new(1920.0, H as f32);
    let mut tree = UITree::new();
    // D4: build under a region (see footer_demo). Full-canvas rect → no-op clip.
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, 1920.0, H as f32),
        ZTier::Chrome,
        "header",
        UIFlags::empty(),
    );
    let start = tree.count();
    header.build(&mut tree, &layout);
    tree.end_region(region, start);

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

/// §18 Phase 3: the inspector-card button skins (`CARD_RAISED` / `CARD_RECESSED`)
/// rendered on the dark card well, so the off-chip + on-fill + hover + press read
/// the same as the chrome `state_button` did before the kit move.
#[test]
fn card_button_skins_sheet() {
    use manifold_ui::chrome::components::{state_button_skinned, StateButtonSkin};

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/card_button_skins.png");

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::EFFECT_CARD_INNER_BG_C32);
    ui.draw_text(
        14.0,
        10.0,
        "CARD BUTTON SKINS (\u{00a7}18 P3)  cols: off  on  hover  press",
        12.0,
        color::TEXT_NORMAL,
    );

    let rows: &[(&str, StateButtonSkin, Color32)] = &[
        ("RAISED  env", StateButtonSkin::CARD_RAISED, color::ENVELOPE_ACTIVE_C32),
        ("RAISED  drv", StateButtonSkin::CARD_RAISED, color::DRIVER_ACTIVE_C32),
        ("RECESSED drv", StateButtonSkin::CARD_RECESSED, color::DRIVER_ACTIVE_C32),
        ("RECESSED abl", StateButtonSkin::CARD_RECESSED, color::ABL_BADGE_C32),
    ];
    for (i, (label, skin, hue)) in rows.iter().enumerate() {
        let y = 44.0 + i as f32 * 30.0;
        ui.draw_text(14.0, y + 4.0, label, 11.0, color::TEXT_NORMAL);
        let off = state_button_skinned(*hue, false, color::FONT_CAPTION, skin);
        let on = state_button_skinned(*hue, true, color::FONT_CAPTION, skin);
        ui.draw_rect(140.0, y, 48.0, 20.0, off.bg_color);
        ui.draw_rect(198.0, y, 48.0, 20.0, on.bg_color);
        ui.draw_rect(256.0, y, 48.0, 20.0, on.hover_bg_color);
        ui.draw_rect(314.0, y, 48.0, 20.0, on.pressed_bg_color);
    }

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "card button skins sheet produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "card-skins-sheet");
    {
        let mut enc = device.create_encoder("card-skins-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("card button skins sheet → {png}");
}

/// §19 Phase 4: the static focus lifts (card well, timeline lane), the
/// `panel_state` empty/error/loading line colours, and the record-button breathe
/// sampled across one cycle — all on one sheet to eyeball the hierarchy + states.
#[test]
fn focus_states_record_sheet() {
    use manifold_ui::chrome::components::{panel_state_style, PanelStateKind};

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/focus_states_record.png");

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);
    ui.draw_text(14.0, 10.0, "FOCUS + STATES + RECORD (\u{00a7}19 P4)", 12.0, color::TEXT_NORMAL);

    // Card well: base vs focused (+1 ramp step), effect + generator, plus the
    // selected accent border.
    let cy = 44.0;
    ui.draw_text(14.0, cy + 4.0, "card well  base | focus", 11.0, color::TEXT_DIMMED_C32);
    ui.draw_rect(180.0, cy, 56.0, 22.0, color::EFFECT_CARD_INNER_BG_C32);
    ui.draw_rect(238.0, cy, 56.0, 22.0, color::lighten(color::EFFECT_CARD_INNER_BG_C32, color::FOCUS_LIFT_STEP));
    ui.draw_rect(304.0, cy, 56.0, 22.0, color::GEN_CARD_INNER_BG_C32);
    ui.draw_rect(362.0, cy, 56.0, 22.0, color::lighten(color::GEN_CARD_INNER_BG_C32, color::FOCUS_LIFT_STEP));
    ui.draw_rect(430.0, cy, 56.0, 22.0, color::SELECTED_BORDER);

    // Timeline lane: even / odd / focused (+1 ramp step).
    let ty = 80.0;
    ui.draw_text(14.0, ty + 4.0, "lane  even | odd | focus", 11.0, color::TEXT_DIMMED_C32);
    ui.draw_rect(180.0, ty, 80.0, 22.0, color::TRACK_BG);
    ui.draw_rect(262.0, ty, 80.0, 22.0, color::TRACK_BG_ALT);
    ui.draw_rect(344.0, ty, 80.0, 22.0, color::lighten(color::TRACK_BG, color::FOCUS_LIFT_STEP));

    // panel_state message colours.
    let sy = 120.0;
    ui.draw_text(14.0, sy + 4.0, "panel_state:", 11.0, color::TEXT_DIMMED_C32);
    ui.draw_text(140.0, sy + 4.0, "Empty hint", 12.0, panel_state_style(PanelStateKind::Empty).text_color);
    ui.draw_text(280.0, sy + 4.0, "Error", 12.0, panel_state_style(PanelStateKind::Error).text_color);
    ui.draw_text(360.0, sy + 4.0, "Loading", 12.0, panel_state_style(PanelStateKind::Loading).text_color);

    // Record breathe: dim → bright across one cycle.
    let ry = 156.0;
    ui.draw_text(14.0, ry + 4.0, "record breathe  dim \u{2192} bright", 11.0, color::TEXT_DIMMED_C32);
    let n = 26;
    for k in 0..n {
        let phase = k as f32 / (n - 1) as f32;
        let c = color::mix(color::RECORD_PULSE_DIM, color::RECORD_PULSE_BRIGHT, phase);
        ui.draw_rect(220.0 + k as f32 * 15.0, ry, 14.0, 22.0, c);
    }

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "focus/states/record sheet produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "focus-states-sheet");
    {
        let mut enc = device.create_encoder("focus-states-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("focus/states/record sheet → {png}");
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
    use manifold_ui::panels::picker_core::PickerItem;
    use manifold_ui::panels::InspectorTab;
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/browser_popup_demo.png");

    let names: Vec<String> = ["Blur", "Glitch", "Edge Stretch", "Feedback", "Chroma", "Pixelate"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let items: Vec<PickerItem> = names
        .iter()
        .map(|n| PickerItem {
            label: n.clone(),
            type_id: n.to_lowercase(),
            category: Some("Stylize".to_string()),
            search_text: None,
            badge: None,
            source: None,
            missing_from_library: false,
            thumbnail: None,
        })
        .collect();

    let mut popup = BrowserPopupPanel::new();
    popup.set_screen_size(W as f32, H as f32);
    popup.open(BrowserPopupRequest {
        mode: BrowserPopupMode::Effect,
        tab: InspectorTab::Layer,
        layer_id: None,
        items,
        category_names: vec!["Stylize".to_string()],
        spawn_graph_pos: None,
        paste_count: 0,
        screen_anchor: Vec2::new(30.0, 40.0),
    });

    let mut tree = UITree::new();
    // D4: the popup mints root-parented nodes; build them inside a region
    // bracket (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D4). Overlay tier matches
    // the modal's real stacking; full-canvas rect → no-op clip.
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Overlay,
        "browser_popup",
        UIFlags::empty(),
    );
    let start = tree.count();
    popup.build(&mut tree);
    tree.end_region(region, start);

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

/// PRESET_LIBRARY P6 headless DISPLAY proof: a browser cell carrying a
/// thumbnail path actually PAINTS the decoded image (the full path — the
/// popup's `add_image` node + the `UIRenderer` textured-quad pipeline + the
/// registered-image cache), not just a flat coloured cell. Registers a REAL
/// committed factory thumbnail, opens the popup with items pointing at it,
/// renders, and writes a PNG to eyeball. This is the one part of P6's
/// in-app display that IS verifiable without a running app.
#[test]
fn browser_popup_thumbnails_paint() {
    use manifold_ui::node::{texture_handle_for_key, Vec2};
    use manifold_ui::panels::browser_popup::{
        BrowserPopupMode, BrowserPopupPanel, BrowserPopupRequest,
    };
    use manifold_ui::panels::picker_core::PickerItem;
    use manifold_ui::panels::InspectorTab;
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    // A real committed factory thumbnail (verified elsewhere to render as a
    // clean Lissajous curve on black). Decode + register it exactly as the
    // app's per-frame thumbnail pass does.
    let thumb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/preset-thumbnails/generators/Lissajous.png");
    let (tw, th, rgba) = manifold_renderer::preset_thumbnail::decode_png_rgba8(&thumb)
        .expect("decode committed Lissajous thumbnail");
    let thumb_path = thumb.to_string_lossy().to_string();
    let handle = texture_handle_for_key(&thumb_path);
    assert!(
        ui.register_image(&device, handle, tw, th, &rgba),
        "thumbnail must register into the UIRenderer image cache"
    );

    let items: Vec<PickerItem> = ["Lissajous", "Plasma", "StarField", "Tesseract"]
        .iter()
        .map(|n| PickerItem {
            label: n.to_string(),
            type_id: n.to_lowercase(),
            category: Some("Stylize".to_string()),
            search_text: None,
            badge: Some("Factory".to_string()),
            source: None,
            missing_from_library: false,
            thumbnail: Some(thumb_path.clone()),
        })
        .collect();

    let mut popup = BrowserPopupPanel::new();
    popup.set_screen_size(W as f32, H as f32);
    popup.open(BrowserPopupRequest {
        mode: BrowserPopupMode::Effect,
        tab: InspectorTab::Layer,
        layer_id: None,
        items,
        category_names: vec!["Stylize".to_string()],
        spawn_graph_pos: None,
        paste_count: 0,
        screen_anchor: Vec2::new(30.0, 40.0),
    });

    let mut tree = UITree::new();
    // D4: build the popup's root-parented nodes inside a region bracket
    // (see browser_popup_demo). Overlay tier; full-canvas rect → no-op clip.
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Overlay,
        "browser_popup",
        UIFlags::empty(),
    );
    let start = tree.count();
    popup.build(&mut tree);
    tree.end_region(region, start);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_3);
    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "popup with thumbnails produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "browser-popup-thumbs");
    {
        let mut enc = device.create_encoder("browser-popup-thumbs-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/browser_popup_thumbnails.png");
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("browser popup thumbnails → {png}");
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

/// Renders GPU clip bodies (§24 5b) across their states — normal / selected /
/// hovered / muted / locked, video + generator — on the dark tracks background,
/// so the rounded gradient body, the normal vs selected border, and the
/// lift-on-select shadow can be eyeballed before the in-app cutover. This is the
/// treatment in isolation; the look itself is tuned in the Phase-6 eye pass.
#[test]
fn clip_body_sheet() {
    use manifold_renderer::clip_draw::{emit_clips, ClipBody};
    use manifold_ui::node::Rect;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/clip_body_sheet.png");

    let video = color::CLIP_NORMAL;
    let gen_c = color::CLIP_GEN_NORMAL;
    let cw = 140.0;
    let ch = 44.0;
    let gap = 14.0;

    // (label, base_color, selected, hovered, muted, locked, generator)
    let cases: &[(&str, Color32, bool, bool, bool, bool, bool)] = &[
        ("normal", video, false, false, false, false, false),
        ("selected", video, true, false, false, false, false),
        ("hovered", video, false, true, false, false, false),
        ("muted", video, false, false, true, false, false),
        ("locked", video, false, false, false, true, false),
        ("gen normal", gen_c, false, false, false, false, true),
        ("gen selected", gen_c, true, false, false, false, true),
    ];

    let mut clips = Vec::new();
    for (i, &(_, base, sel, hov, mut_, lock, g)) in cases.iter().enumerate() {
        let row = i / 4;
        let coln = i % 4;
        let x = 24.0 + coln as f32 * (cw + gap);
        let y = 64.0 + row as f32 * (ch + 44.0);
        clips.push(ClipBody {
            rect: Rect::new(x, y, cw, ch),
            base_color: base,
            selected: sel,
            hovered: hov,
            muted: mut_,
            locked: lock,
            generator: g,
            alpha: 1.0,
        });
    }

    // Build matching ClipScreenRects so the name labels render through the real
    // emitter (dark-on-light / light-on-dark by body luminance, scissor-clipped).
    use manifold_renderer::clip_draw::emit_clip_names;
    use manifold_ui::panels::viewport::ClipScreenRect;
    let names = [
        "TEXT BOT L",
        "FLOWERS",
        "a-very-long-clip-name-that-clips",
        "DRUMS.wav",
        "locked",
        "Tesseract",
        "NestedCubes",
    ];
    let mut name_rects = Vec::new();
    for (i, c) in clips.iter().enumerate() {
        name_rects.push(ClipScreenRect {
            clip_id: manifold_foundation::ClipId::new(format!("c{i}")),
            layer_index: 0,
            rect: c.rect,
            base_color: c.base_color,
            name: names[i].into(),
            start_beat: manifold_foundation::Beats::ZERO,
            end_beat: manifold_foundation::Beats::ONE,
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: false,
            waveform: None,
            in_point_seconds: 0.0,
            waveform_breakpoints: Vec::new(),
        });
    }

    ui.begin_frame();
    // Dark tracks background so the bodies sit on the real timeline tone.
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_0);
    ui.draw_text(24.0, 14.0, "GPU CLIP BODIES + NAMES (\u{00a7}24 5b)", 13.0, color::TEXT_NORMAL);
    emit_clips(&mut ui, &clips);
    // Full-surface tracks rect — the swatch grid is the whole tile, so this just
    // satisfies the viewport-clamp arg without cropping any name.
    emit_clip_names(&mut ui, &name_rects, Rect::new(0.0, 0.0, W as f32, H as f32));
    // State labels above each clip.
    for (i, &(label, ..)) in cases.iter().enumerate() {
        let row = i / 4;
        let coln = i % 4;
        let x = 24.0 + coln as f32 * (cw + gap);
        let y = 64.0 + row as f32 * (ch + 44.0);
        ui.draw_text(x, y - 16.0, label, 11.0, color::TEXT_DIMMED);
    }

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "clip body sheet produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "clip-body-sheet");
    {
        let mut enc = device.create_encoder("clip-body-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("clip body sheet → {png}");
}

/// Renders audio-clip bodies with their waveform painted INSIDE the body via the
/// per-clip GPU content path (§24 5b) — so the in-clip waveform (spectral colour,
/// rounded-corner inset, sitting on the gradient body) can be eyeballed headlessly.
/// Covers a wide clip, a narrow clip, and a selected clip.
#[test]
fn clip_waveform_sheet() {
    use manifold_renderer::clip_content_gpu::ClipContentGpu;
    use manifold_renderer::clip_draw::{emit_clips, ClipBody};
    use manifold_ui::node::Rect;
    use manifold_ui::panels::viewport::ClipScreenRect;
    use manifold_ui::waveform_renderer::WaveformRenderer;
    use std::sync::Arc;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let mut content = ClipContentGpu::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/clip_waveform_sheet.png");

    // One second of a loud-ish sine sweep → visible, spectrally-varied bars.
    let samples: Vec<f32> = (0..44_100)
        .map(|i| (i as f32 / (8.0 + i as f32 / 4000.0)).sin() * 0.85)
        .collect();
    let mut wr = WaveformRenderer::new();
    wr.set_audio_data(&samples, 1, 44_100);
    assert!(wr.is_ready(), "synthetic waveform should be ready");
    let wf = Arc::new(wr);

    // (label, x, y, w, selected)
    let cases: &[(&str, f32, f32, f32, bool)] = &[
        ("wide audio clip", 24.0, 60.0, 380.0, false),
        ("narrow", 430.0, 60.0, 90.0, false),
        ("selected audio clip", 24.0, 180.0, 380.0, true),
    ];
    let tracks = Rect::new(0.0, 40.0, W as f32, H as f32 - 40.0);
    let ch = 88.0;

    let mut bodies = Vec::new();
    let mut clips = Vec::new();
    for (i, &(_, x, y, w, sel)) in cases.iter().enumerate() {
        let rect = Rect::new(x, y, w, ch);
        bodies.push(ClipBody {
            rect,
            base_color: color::CLIP_NORMAL,
            selected: sel,
            hovered: false,
            muted: false,
            locked: false,
            generator: false,
            alpha: 1.0,
        });
        clips.push(ClipScreenRect {
            clip_id: manifold_foundation::ClipId::new(format!("a{i}")),
            layer_index: 0,
            rect,
            base_color: color::CLIP_NORMAL,
            name: "".into(),
            start_beat: manifold_foundation::Beats::ZERO,
            end_beat: manifold_foundation::Beats::from_f32(4.0),
            is_muted: false,
            is_locked: false,
            is_generator: false,
            is_audio: true,
            waveform: Some(wf.clone()),
            in_point_seconds: 0.0,
            // 4 beats × 0.25 s/beat = 1.0 s → whole file maps across the clip.
            waveform_breakpoints: vec![(0.0, 0.0), (1.0, 1.0)],
        });
    }

    // Bodies first (Clear), then the per-clip waveform textures on top (Load).
    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_0);
    ui.draw_text(24.0, 14.0, "GPU IN-CLIP WAVEFORMS (\u{00a7}24 5b)", 13.0, color::TEXT_NORMAL);
    emit_clips(&mut ui, &bodies);
    for (i, &(label, x, y, ..)) in cases.iter().enumerate() {
        let _ = i;
        ui.draw_text(x, y - 16.0, label, 11.0, color::TEXT_DIMMED);
    }
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "clip waveform sheet produced no body draws");

    let target = RenderTarget::new(&device, W, H, FORMAT, "clip-waveform-sheet");
    {
        let mut enc = device.create_encoder("clip-waveform-bodies");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    {
        let mut enc = device.create_encoder("clip-waveform-content");
        content.render(&device, &mut enc, &target.texture, W, H, 1.0, tracks, &clips);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("clip waveform sheet → {png}");
}

/// Renders clip thumbnails (§24 5c): blits cells of a synthetic content→UI atlas
/// into clip bodies via `ClipThumbGpu`, masked to the rounded clip shape. Verifies
/// the WGSL→Metal pipeline compiles and that the thumbnail fills the body with
/// rounded corners (no square nibs over the round clip).
#[test]
fn clip_thumbnail_sheet() {
    use manifold_renderer::clip_thumb_gpu::{ClipThumbGpu, ThumbQuad};
    use manifold_ui::node::Rect;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let mut thumb = ClipThumbGpu::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/clip_thumbnail_sheet.png");

    // Synthetic 2×2-cell atlas (256×256). Each cell gets a distinct, clearly-an-image
    // pattern so a thumbnail is obviously not a solid fill.
    const AW: u32 = 256;
    const AH: u32 = 256;
    let mut atlas_px = vec![Color32::TRANSPARENT; (AW * AH) as usize];
    for y in 0..AH {
        for x in 0..AW {
            let (cx, cy) = (x / 128, y / 128); // which cell
            let fx = (x % 128) as f32 / 128.0;
            let fy = (y % 128) as f32 / 128.0;
            let c = match (cx, cy) {
                (0, 0) => Color32::new((fx * 255.0) as u8, (fy * 255.0) as u8, 200, 255), // gradient
                (1, 0) => Color32::new(230, (fx * 255.0) as u8, 40, 255),                 // orange ramp
                (0, 1) => Color32::new(40, 200, (fy * 255.0) as u8, 255),                 // teal ramp
                _ => {
                    let on = ((x / 16 + y / 16) % 2) == 0; // checker
                    if on { Color32::new(235, 235, 240, 255) } else { Color32::new(30, 30, 36, 255) }
                }
            };
            atlas_px[(y * AW + x) as usize] = c;
        }
    }
    let atlas = device.create_texture(&manifold_gpu::GpuTextureDesc {
        width: AW,
        height: AH,
        depth: 1,
        format: GpuTextureFormat::Rgba8UnormSrgb,
        dimension: manifold_gpu::GpuTextureDimension::D2,
        usage: manifold_gpu::GpuTextureUsage::SHADER_READ | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
        label: "test atlas",
        mip_levels: 1,
    });
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(atlas_px.as_ptr() as *const u8, atlas_px.len() * 4) };
    device.upload_texture(&atlas, bytes);

    // One clip per atlas cell, plus a small clip, all rounded (radius 4).
    let ch = 80.0;
    let cells = [
        ("gradient", [0.0, 0.0], [0.5, 0.5]),
        ("orange", [0.5, 0.0], [1.0, 0.5]),
        ("teal", [0.0, 0.5], [0.5, 1.0]),
        ("checker", [0.5, 0.5], [1.0, 1.0]),
    ];
    let mut quads = Vec::new();
    for (i, (_, uv_min, uv_max)) in cells.iter().enumerate() {
        let col = i % 2;
        let row = i / 2;
        let x = 24.0 + col as f32 * 320.0;
        let y = 60.0 + row as f32 * 110.0;
        // Single still: geometry == body, so the whole rounded rect is one cell.
        let r = Rect::new(x, y, 280.0, ch);
        quads.push(ThumbQuad {
            rect: r,
            body_rect: r,
            radius: 4.0,
            uv_min: *uv_min,
            uv_max: *uv_max,
        });
    }
    // A small clip to confirm radius clamps gracefully.
    let small = Rect::new(24.0, 290.0, 40.0, ch);
    quads.push(ThumbQuad {
        rect: small,
        body_rect: small,
        radius: 4.0,
        uv_min: [0.0, 0.0],
        uv_max: [0.5, 0.5],
    });

    // A FILMSTRIP clip (§24 5c-2): four bar cells tiled across one body, each
    // sampling a different atlas cell. All share the same `body_rect`, so the
    // interior seams stay square and only the outer corners round.
    let strip_body = Rect::new(24.0 + 320.0, 290.0, 280.0, ch);
    let strip_cells = [
        ([0.0, 0.0], [0.5, 0.5]),
        ([0.5, 0.0], [1.0, 0.5]),
        ([0.0, 0.5], [0.5, 1.0]),
        ([0.5, 0.5], [1.0, 1.0]),
    ];
    let cell_w = strip_body.width / strip_cells.len() as f32;
    for (i, (uv_min, uv_max)) in strip_cells.iter().enumerate() {
        quads.push(ThumbQuad {
            rect: Rect::new(strip_body.x + i as f32 * cell_w, strip_body.y, cell_w, ch),
            body_rect: strip_body,
            radius: 4.0,
            uv_min: *uv_min,
            uv_max: *uv_max,
        });
    }

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_0);
    ui.draw_text(24.0, 14.0, "GPU CLIP THUMBNAILS (\u{00a7}24 5c)", 13.0, color::TEXT_NORMAL);
    for (i, (label, ..)) in cells.iter().enumerate() {
        let col = i % 2;
        let row = i / 2;
        let x = 24.0 + col as f32 * 320.0;
        let y = 60.0 + row as f32 * 110.0;
        ui.draw_text(x, y - 16.0, label, 11.0, color::TEXT_DIMMED);
    }
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "thumbnail sheet produced no bg draws");

    let target = RenderTarget::new(&device, W, H, FORMAT, "clip-thumbnail-sheet");
    {
        let mut enc = device.create_encoder("clip-thumb-bg");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    {
        let mut enc = device.create_encoder("clip-thumb-cells");
        let tracks = Rect::new(0.0, 0.0, W as f32, H as f32);
        thumb.render(&device, &mut enc, &target.texture, W, H, 1.0, tracks, &atlas, &quads);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("clip thumbnail sheet → {png}");
}

#[test]
fn box_downsample_averages_high_frequency() {
    // The §24 5c-2 P5 capture downsample must AVERAGE a high-frequency source into
    // a cell, not point-sample it (which would alias to an extreme). Downsample a
    // 256×256 1px checkerboard into 64×64 and assert the centre reads mid-grey.
    use manifold_renderer::clip_thumb_gpu::create_box_downsample_pipeline;

    let device = GpuDevice::new();
    let pipe = create_box_downsample_pipeline(&device, FORMAT, 64, 64);
    let sampler = device.create_sampler(&manifold_gpu::GpuSamplerDesc {
        min_filter: manifold_gpu::GpuFilterMode::Linear,
        mag_filter: manifold_gpu::GpuFilterMode::Linear,
        ..Default::default()
    });

    const SS: u32 = 256;
    let mut px = vec![0u8; (SS * SS * 4) as usize];
    for y in 0..SS {
        for x in 0..SS {
            let v: u8 = if (x + y) & 1 == 0 { 255 } else { 0 };
            let i = ((y * SS + x) * 4) as usize;
            px[i] = v;
            px[i + 1] = v;
            px[i + 2] = v;
            px[i + 3] = 255;
        }
    }
    let src = device.create_texture(&manifold_gpu::GpuTextureDesc {
        width: SS,
        height: SS,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: manifold_gpu::GpuTextureDimension::D2,
        usage: manifold_gpu::GpuTextureUsage::SHADER_READ | manifold_gpu::GpuTextureUsage::CPU_UPLOAD,
        label: "ds-source",
        mip_levels: 1,
    });
    device.upload_texture(&src, &px);

    let target = RenderTarget::new(&device, 64, 64, FORMAT, "ds-target");
    {
        let mut enc = device.create_encoder("ds-blit");
        enc.draw_fullscreen(
            &pipe,
            &target.texture,
            &[
                manifold_gpu::GpuBinding::Texture { binding: 0, texture: &src },
                manifold_gpu::GpuBinding::Sampler { binding: 1, sampler: &sampler },
            ],
            true,
            true,
            "ds-blit",
        );
        enc.commit_and_wait_completed();
    }
    let bytes = readback_w(&device, &target.texture, 64, 64);
    let i = ((32 * 64 + 32) * 4) as usize;
    let r = bytes[i];
    assert!(
        (64..=192).contains(&r),
        "box downsample of a checkerboard should read mid-grey, got {r}"
    );
}

/// Renders every atlas icon (§24 5d/5e) — the 5 waveforms, the cog, the four
/// layer-type badges (video play / generator starburst / group folder / audio
/// bars), and the playhead head triangle — each on a dark tile and on a
/// layer-colour tile (contrast-coloured, as drawn in the header), so the glyph
/// shapes and their on-colour legibility can be eyeballed headlessly.
#[test]
fn icon_badge_sheet() {
    use manifold_ui::icons::Icon;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/icon_badge_sheet.png");

    let icons: &[(&str, Icon)] = &[
        ("WaveSine", Icon::WaveSine),
        ("WaveTriangle", Icon::WaveTriangle),
        ("WaveSawtooth", Icon::WaveSawtooth),
        ("WaveSquare", Icon::WaveSquare),
        ("WaveRandom", Icon::WaveRandom),
        ("Cog", Icon::Cog),
        ("LayerVideo (play)", Icon::LayerVideo),
        ("LayerGenerator (starburst)", Icon::LayerGenerator),
        ("LayerGroup (folder)", Icon::LayerGroup),
        ("LayerAudio (bars)", Icon::LayerAudio),
        ("Playhead (triangle)", Icon::Playhead),
    ];

    // A layer-colour tile to check the badge's contrast colour (as in the header).
    let layer_tile = Color32::new(120, 90, 160, 255);

    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_1);
    ui.draw_text(14.0, 8.0, "ATLAS ICONS (\u{00a7}24 5d/5e)", 13.0, color::TEXT_NORMAL);
    ui.draw_text(14.0, 26.0, "dark tile | layer-colour tile (contrast)", 11.0, color::TEXT_DIMMED);

    let tile = 34.0;
    for (i, (label, icon)) in icons.iter().enumerate() {
        let y = 44.0 + i as f32 * 44.0;
        // On a dark tracks tile.
        ui.draw_rect(14.0, y, tile, tile, color::BG_0);
        ui.draw_icon(icon.id(), 17.0, y + 3.0, tile - 6.0, tile - 6.0, color::TEXT_WHITE_C32, None);
        // On a layer-colour tile, contrast-coloured (how the header draws badges).
        ui.draw_rect(58.0, y, tile, tile, layer_tile);
        ui.draw_icon(
            icon.id(),
            61.0,
            y + 3.0,
            tile - 6.0,
            tile - 6.0,
            color::contrast_text_color(layer_tile),
            None,
        );
        // Small badge-size sample (13px) to match the header badge.
        ui.draw_rect(102.0, y + 10.0, 16.0, 16.0, layer_tile);
        ui.draw_icon(
            icon.id(),
            103.0,
            y + 11.0,
            color::LAYER_CTRL_TYPE_BADGE_SIZE,
            color::LAYER_CTRL_TYPE_BADGE_SIZE,
            color::contrast_text_color(layer_tile),
            None,
        );
        ui.draw_text(130.0, y + 10.0, label, 12.0, color::TEXT_NORMAL);
    }

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "icon badge sheet produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "icon-badge-sheet");
    {
        let mut enc = device.create_encoder("icon-badge-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("icon badge sheet → {png}");
}

/// Renders the §24 5e "now + nav" elements in a mock timeline: the playhead (red
/// line + downward triangle head at the ruler top) next to the blue insert cursor
/// (single-row bar + ruler square), and the horizontal scrollbar (track + rounded
/// thumb) in its reserved strip — so the unmissable-now treatment and the
/// scrollbar look can be eyeballed headlessly.
#[test]
fn playhead_scrollbar_demo() {
    use manifold_ui::icons::Icon;

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/playhead_scrollbar_demo.png");

    let ruler_y = 24.0;
    let ruler_h = color::RULER_HEIGHT;
    let tracks_top = ruler_y + ruler_h;
    let sb_h = color::TIMELINE_SCROLLBAR_HEIGHT;
    let tracks_bottom = H as f32 - sb_h;

    ui.begin_frame();
    // Tracks background + ruler band.
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_0);
    ui.draw_rect(0.0, ruler_y, W as f32, ruler_h, color::HEADER_BG);
    ui.draw_text(14.0, 6.0, "PLAYHEAD + INSERT CURSOR + SCROLLBAR (\u{00a7}24 5e)", 13.0, color::TEXT_NORMAL);

    // Playhead — red line spanning ruler→tracks, capped by a triangle head.
    let px = 230.0;
    ui.draw_rect(
        px - 1.0,
        ruler_y,
        color::PLAYHEAD_WIDTH,
        tracks_bottom - ruler_y,
        color::PLAYHEAD_RED,
    );
    let s = color::PLAYHEAD_HEAD_SIZE;
    ui.draw_icon(Icon::Playhead.id(), px - s * 0.5, ruler_y, s, s, color::PLAYHEAD_RED, None);
    ui.draw_text(px + 10.0, ruler_y + ruler_h + 6.0, "playhead", 11.0, color::PLAYHEAD_RED);

    // Insert cursor — blue single-row bar + small ruler square (subordinate).
    let cx = 430.0;
    let row_y = tracks_top + 70.0;
    ui.draw_rect(cx, row_y, 2.0, 60.0, color::INSERT_CURSOR_BLUE);
    let ms = color::INSERT_CURSOR_RULER_MARKER_SIZE;
    ui.draw_rect(cx - ms * 0.5, ruler_y + ruler_h - ms, ms, ms, color::INSERT_CURSOR_BLUE);
    ui.draw_text(cx + 8.0, row_y, "insert cursor", 11.0, color::INSERT_CURSOR_BLUE);

    // Horizontal scrollbar — track + rounded thumb (40% wide, 15% in), like the
    // viewport's `scrollbar_h_layout`.
    let sb_y = H as f32 - sb_h;
    ui.draw_rect(0.0, sb_y, W as f32, sb_h, color::SCROLLBAR_TRACK_C32);
    let inset = color::TIMELINE_SCROLLBAR_THUMB_INSET;
    let thumb_w = W as f32 * 0.4;
    let thumb_x = W as f32 * 0.15;
    let thumb_h = sb_h - inset * 2.0;
    ui.draw_rounded_rect(
        thumb_x,
        sb_y + inset,
        thumb_w,
        thumb_h,
        color::SCROLLBAR_THUMB_C32,
        thumb_h * 0.5,
    );

    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "playhead/scrollbar demo produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "playhead-scrollbar-demo");
    {
        let mut enc = device.create_encoder("playhead-scrollbar-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("playhead scrollbar demo → {png}");
}

/// Renders the three modulation drawers (Trigger / LFO / Audio) built through the
/// new `chrome::Theme` context, each above a blue param slider — so the settled
/// design can be eyeballed: a source-tinted dark surface (no border), white text,
/// source-coloured option fills + slider fills + spine, distinct from the blue
/// param slider it operates over. This is the API's first consumer.
#[test]
fn modulation_drawer_sheet() {
    use manifold_ui::chrome::Theme;
    use manifold_ui::node::Rect;
    use manifold_ui::panels::drawer::{self, ButtonWidth, DrawerButton, DrawerRow, DrawerSpec};
    use manifold_ui::panels::PanelAction;
    use manifold_ui::slider::{BitmapSlider, SliderColors};
    use manifold_ui::{UIFlags, UITree, ZTier};

    // Headless swatch render: sliders never receive a right-click, so any valid
    // reset action serves as a placeholder for the now-required field/param.
    let placeholder_reset = || {
        PanelAction::slider_reset(
            PanelAction::MasterOpacitySnapshot,
            PanelAction::MasterOpacityChanged(1.0),
            PanelAction::MasterOpacityCommit,
        )
    };

    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/modulation_drawer_sheet.png");

    let bf = color::FONT_BODY;
    let sf = color::FONT_BODY;
    let btn = |l: &str, a: bool| DrawerButton::new(l, a);
    let buttons = |bs: Vec<DrawerButton>, label: Option<&str>, w: ButtonWidth| DrawerRow::Buttons {
        buttons: bs,
        width: w,
        label: label.map(|s| s.to_string()),
    };
    let slider = |label: &str, norm: f32, value: &str| DrawerRow::Slider {
        label: label.to_string(),
        norm,
        value_text: value.to_string(),
        label_w: 52.0,
        default_norm: norm,
        reset: placeholder_reset(),
        show_meter: false,
    };

    // Trigger (orange) — a single Decay slider.
    let trigger = DrawerSpec {
        rows: vec![slider("Decay", 0.40, "1.00")],
        btn_font_size: bf,
        slider_font_size: sf,
        theme: Theme::INSPECTOR.with_accent(color::ENVELOPE_ACTIVE_C32).tinted(),
    };

    // LFO (driver) — rate grid + feel + shapes.
    let rate: Vec<DrawerButton> = ["1/32", "1/16", "1/8", "1/4", "1/2", "1", "2", "4", "8", "16", "32", "Free"]
        .iter()
        .enumerate()
        .map(|(i, l)| btn(l, i == 8))
        .collect();
    let lfo = DrawerSpec {
        rows: vec![
            buttons(rate, None, ButtonWidth::Uniform),
            buttons(
                vec![btn("Straight", true), btn("Dotted", false), btn("Triplet", false)],
                None,
                ButtonWidth::Uniform,
            ),
            buttons(
                vec![btn("Sin", false), btn("Tri", false), btn("Saw", true), btn("Sqr", false), btn("Rnd", false), btn("Invert", false)],
                None,
                ButtonWidth::Uniform,
            ),
        ],
        btn_font_size: bf,
        slider_font_size: sf,
        theme: Theme::INSPECTOR.with_accent(color::DRIVER_ACTIVE_C32).tinted(),
    };

    // Audio (green) — selectors + shaping sliders.
    let audio = DrawerSpec {
        rows: vec![
            buttons(vec![btn("Audio 1", true)], Some("Source"), ButtonWidth::Proportional),
            buttons(
                vec![btn("Amp", true), btn("Centroid", false), btn("Noise", false), btn("Flux", false), btn("Trans", false)],
                Some("Feature"),
                ButtonWidth::Uniform,
            ),
            buttons(
                vec![btn("Full", true), btn("Low", false), btn("Mid", false), btn("High", false)],
                Some("Band"),
                ButtonWidth::Uniform,
            ),
            buttons(vec![btn("Inv", false), btn("Delta", false)], None, ButtonWidth::Proportional),
            slider("Amount", 0.55, "1.00"),
            slider("Attack", 0.06, "5 ms"),
            slider("Release", 0.14, "120 ms"),
        ],
        btn_font_size: bf,
        slider_font_size: sf,
        theme: Theme::INSPECTOR.with_accent(color::AUDIO_TRIM_BAR_C32).tinted(),
    };

    let groups: [(&str, &DrawerSpec); 3] =
        [("TRIGGER (envelope)", &trigger), ("LFO (driver)", &lfo), ("AUDIO", &audio)];

    let x = 24.0;
    let dw = 600.0;
    let mut tree = UITree::new();

    ui.begin_frame();
    // Sit on the dark card inner well, as the param rows do in-app.
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::EFFECT_CARD_INNER_BG_C32);
    ui.draw_text(
        14.0,
        8.0,
        "MOD CARD via Theme  (slider + drawer = ONE source-tinted card; param fill = blue)",
        12.0,
        color::TEXT_NORMAL,
    );

    // Immediate-mode mod cards (rendered before render_tree, so they sit behind the
    // slider + drawer nodes — exactly how build_param_row draws the card first).
    let row_h = 24.0;
    let slider_gap = 6.0; // ROW_SPACING
    let indent = color::SPACE_L; // DRAWER_INDENT
    let mut y = 30.0;
    let mut placed: Vec<(f32, f32, &DrawerSpec)> = Vec::new();
    for (label, spec) in groups {
        ui.draw_text(x, y, label, 11.0, color::TEXT_DIMMED_C32);
        y += 16.0;
        let card_top = y;
        let pad = 4.0; // mirrors param_slider_shared::MOD_CARD_PAD
        let card_h = pad + row_h + slider_gap + spec.height();
        // One source-tinted card, no spine, behind the whole param. Padded out on
        // top + left + right so content sits inset (top also covers trim handles).
        ui.draw_rounded_rect(
            x - pad,
            card_top - pad,
            dw + pad * 2.0,
            card_h,
            spec.theme.surface,
            color::CARD_RADIUS,
        );
        placed.push((card_top, card_h, spec));
        y += card_h + 18.0;
    }

    // Now the slider + drawer nodes on top of each card. D4: these mint
    // root-parented nodes, so build them inside a region bracket
    // (`UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` D1/D4). Full-canvas rect → no-op
    // clip; the immediate-mode cards above stay behind (drawn pre-render_tree).
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        ZTier::Base,
        "mod_drawer_sheet",
        UIFlags::empty(),
    );
    let region_start = tree.count();
    for (card_top, _card_h, spec) in placed {
        let _ = BitmapSlider::build(
            &mut tree,
            None,
            Rect::new(x, card_top, dw, row_h),
            Some("Amount"),
            0.62,
            "0.65",
            &SliderColors::default_slider(),
            sf,
            52.0,
            0.62,
            placeholder_reset(),
        );
        drawer::build(
            &mut tree,
            None,
            x + indent,
            card_top + row_h + slider_gap,
            dw - indent,
            spec,
        );
    }
    tree.end_region(region, region_start);

    ui.render_tree(&tree, None);
    let drew = ui.prepare(&device, W, H, 1.0);
    assert!(drew, "modulation drawer sheet produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "mod-drawer-sheet");
    {
        let mut enc = device.create_encoder("mod-drawer-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("modulation drawer sheet → {png}");
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
