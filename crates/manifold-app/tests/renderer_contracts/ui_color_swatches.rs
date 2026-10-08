//! Colour contact sheet — renders the section 15 semantic ramp and the state colours
//! re-pointed onto it to a PNG, so the ramp can be eyeballed headlessly (no
//! running app). Reuses the same windowless render path as the headless UI
//! spike (docs section 23): `GpuDevice::new()` → `UIRenderer` immediate draws →
//! texture readback → PNG.
//!
//! Run: `SWATCH_OUT=/some/dir cargo test -p manifold-nodes --test ui_color_swatches`
//! then open `$SWATCH_OUT/color_ramp.png`.

#![cfg(target_os = "macos")]

use manifold_nodes as _;
use std::ffi::c_void;
use std::path::Path;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_compositor::display_capture::{AlphaInterpretation, LinearUiReadback};
use manifold_compositor::presentation::UI_FORMAT;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_ui_paint::ui_renderer::UIRenderer;
use manifold_ui::color;

// W*8 must be 256-byte aligned for the RGBA16Float texture→buffer readback copy.
// 640*8 = 5120 = 20*256. H is unconstrained.
const W: u32 = 640;
const H: u32 = 640;
const FORMAT: GpuTextureFormat = UI_FORMAT;

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

    let device = GpuDevice::new_queued("ui_color_swatches");
    let mut ui = UIRenderer::new(&device, FORMAT);

    // A real committed factory thumbnail (verified elsewhere to render as a
    // clean Lissajous curve on black). Decode + register it exactly as the
    // app's per-frame thumbnail pass does.
    let thumb = std::path::Path::new(manifold_nodes::testkit::assets::CATALOG_ASSETS_ROOT)
        .join("preset-thumbnails/generators/Lissajous.png");
    let (tw, th, rgba) = manifold_compositor::preset_thumbnail::decode_png_rgba8(&thumb)
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
            source: None,
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
    save_capture(&png, &bytes, W, H);
    eprintln!("browser popup thumbnails → {png}");
}

/// The Text generator's font picker: the one-column searchable list with
/// every row drawn in the installed font it names, opened on the current
/// font. Writes `font_list_picker.png` to eyeball.
#[test]
fn font_list_picker_paints() {
    use manifold_ui::node::Vec2;
    use manifold_ui::panels::browser_popup::{
        ActionListOptions, BrowserPopupMode, BrowserPopupPanel, BrowserPopupRequest,
    };
    use manifold_ui::panels::picker_core::PickerItem;
    use manifold_ui::panels::{InspectorTab, PanelAction};
    use manifold_ui::{ParamsAction, Rect, UIFlags, UITree, ZTier};

    let device = GpuDevice::new_queued("ui_color_swatches");
    let mut ui = UIRenderer::new(&device, FORMAT);

    let families = manifold_nodes_image::text_rasterizer::TextRasterizer::available_font_families();
    let current = families.iter().position(|f| f == "Georgia");
    let items: Vec<PickerItem> = families
        .iter()
        .map(|f| PickerItem {
            label: f.clone(),
            type_id: f.clone(),
            category: None,
            search_text: None,
            source: None,
            thumbnail: None,
        })
        .collect();
    let actions = families
        .iter()
        .map(|f| PanelAction::Params(ParamsAction::GenStringParamSelected(0, f.clone())))
        .collect();

    let mut popup = BrowserPopupPanel::new();
    popup.set_screen_size(W as f32, H as f32);
    popup.open_actions(
        BrowserPopupRequest {
            mode: BrowserPopupMode::Actions,
            tab: InspectorTab::Layer,
            layer_id: None,
            items,
            category_names: Vec::new(),
            spawn_graph_pos: None,
            paste_count: 0,
            screen_anchor: Vec2::new(30.0, 40.0),
        },
        actions,
        ActionListOptions { empty_label: "No fonts match", label_in_own_font: true, current },
    );

    let mut tree = UITree::new();
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
    assert!(ui.prepare(&device, W, H, 1.0), "font picker produced no draw commands");
    let target = RenderTarget::new(&device, W, H, FORMAT, "font-list-picker");
    {
        let mut enc = device.create_encoder("font-list-picker-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    let bytes = readback(&device, &target.texture);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/font_list_picker.png");
    save_capture(&png, &bytes, W, H);
    eprintln!("font list picker → {png}");
}

/// Renders audio-clip bodies with their waveform painted INSIDE the body via the
/// per-clip GPU content path (section 24 5b) — so the in-clip waveform (spectral colour,
/// rounded-corner inset, sitting on the gradient body) can be eyeballed headlessly.
/// Covers overview, beat-level, and close-up source windows with the real GPU
/// waveform texture path.
#[test]
fn clip_waveform_sheet() {
    use manifold_ui_paint::clip_content_gpu::ClipContentGpu;
    use manifold_ui_paint::clip_draw::{emit_clips, ClipBody};
    use manifold_ui::node::Rect;
    use manifold_ui::panels::viewport::ClipScreenRect;
    use manifold_ui::waveform_renderer::WaveformRenderer;
    use std::sync::Arc;
    use std::time::Instant;

    let device = GpuDevice::new_queued("ui_color_swatches");
    let mut ui = UIRenderer::new(&device, FORMAT);
    let mut content = ClipContentGpu::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/clip_waveform_sheet.png");

    // Keep the artifact self-contained by default. The three components make
    // the spectral palette legible at every zoom: low blue pulses, sustained
    // 800 Hz amber, and short 6 kHz white hits, followed by a quiet tail.
    let (samples, sample_rate, channels) = if let Ok(path) = std::env::var("WAVEFORM_AUDIO_FILE") {
        let decoded = manifold_playback::audio_decoder::decode_audio_to_pcm(&path)
            .unwrap_or_else(|error| panic!("WAVEFORM_AUDIO_FILE {path:?} failed: {error}"));
        (decoded.samples, decoded.sample_rate, decoded.channels)
    } else {
        let sample_rate = 44_100;
        let samples = (0..sample_rate * 8)
            .map(|i| {
                let t = i as f32 / sample_rate as f32;
                let pulse = ((t * 1.5 * std::f32::consts::TAU).sin() * 0.5 + 0.5).powi(8);
                let low = (t * 60.0 * std::f32::consts::TAU).sin() * 0.72 * pulse;
                let mid_gate = if (0.35..6.8).contains(&t) { 0.32 } else { 0.0 };
                let mid = (t * 800.0 * std::f32::consts::TAU).sin() * mid_gate;
                let hit_phase = (t * 2.25).fract();
                let high_gate = if hit_phase < 0.045 { (1.0 - hit_phase / 0.045).sqrt() } else { 0.0 };
                let high = (t * 6000.0 * std::f32::consts::TAU).sin() * 0.62 * high_gate;
                (low + mid + high).clamp(-1.0, 1.0)
            })
            .collect();
        (samples, sample_rate, 1)
    };
    let analysis_start = Instant::now();
    let mut wr = WaveformRenderer::new();
    wr.set_audio_data(&samples, channels, sample_rate);
    assert!(wr.is_ready(), "synthetic waveform should be ready");
    let duration = samples.len() as f32 / channels.max(1) as f32 / sample_rate as f32;
    eprintln!(
        "waveform source: duration={duration:.3}s channels={channels} analysis={:?} storage={}B",
        analysis_start.elapsed(),
        wr.storage_bytes()
    );
    let wf = Arc::new(wr);

    // (label, y, source-start, source-end)
    let end = duration.max(0.001);
    let cases: &[(&str, f32, f32, f32)] = &[
        ("overview · low blue / mid amber / high white", 58.0, 0.0, end),
        ("beat-level · low blue / mid amber / high white", 248.0, end * 0.20, end * 0.38),
        ("close-up · low blue / mid amber / high white", 438.0, end * 0.64, end * 0.72),
    ];
    let tracks = Rect::new(0.0, 40.0, W as f32, H as f32 - 40.0);
    let x = 24.0;
    let cw = W as f32 - 48.0;
    let ch = 150.0;

    let mut bodies = Vec::new();
    let mut clips = Vec::new();
    for (i, &(_, y, source_start, source_end)) in cases.iter().enumerate() {
        let rect = Rect::new(x, y, cw, ch);
        bodies.push(ClipBody {
            rect,
            base_color: color::CLIP_NORMAL,
            selected: i == 1,
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
            in_point_seconds: source_start,
            waveform_breakpoints: vec![(0.0, source_start), (1.0, source_end)],
        });
    }

    // Bodies first (Clear), then the per-clip waveform textures on top (Load).
    ui.begin_frame();
    ui.draw_rect(0.0, 0.0, W as f32, H as f32, color::BG_0);
    ui.draw_text(24.0, 14.0, "GPU IN-CLIP WAVEFORMS · overview / beat-level / close-up", 13.0, color::TEXT_NORMAL);
    emit_clips(&mut ui, &bodies);
    for &(label, y, ..) in cases {
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
    let capture = LinearUiReadback::from_bytes(
        &bytes,
        W,
        H,
        FORMAT,
        AlphaInterpretation::PremultipliedOverBlack,
    )
    .expect("waveform readback shape");
    let pixels = capture.to_srgb_rgba8();
    let mut blue = 0usize;
    let mut amber = 0usize;
    let mut white = 0usize;
    for &(_, y, _, _) in cases {
        let y0 = y as usize + 4;
        let y1 = (y + ch - 30.0) as usize;
        for row in y0..y1.min(H as usize) {
            for col in x as usize..(x + cw) as usize {
                let p = &pixels.as_bytes()[(row * W as usize + col) * 4..][..4];
                let (r, g, b) = (p[0], p[1], p[2]);
                blue += usize::from(b > 150 && r < 150 && g < 210);
                amber += usize::from(r > 160 && g > 100 && b < 160);
                white += usize::from(r > 200 && g > 200 && b > 200);
            }
        }
    }
    assert!(blue > 10, "waveform blue band did not reach the GPU readback");
    assert!(amber > 10, "waveform amber band did not reach the GPU readback");
    assert!(white > 10, "waveform white band did not reach the GPU readback");
    save_capture(&png, &bytes, W, H);
    eprintln!("clip waveform sheet → {png}");
}

#[test]
fn box_downsample_averages_high_frequency() {
    // The section 24 5c-2 P5 capture downsample must AVERAGE a high-frequency source into
    // a cell, not point-sample it (which would alias to an extreme). Downsample a
    // 256×256 1px checkerboard into 64×64 and assert the centre reads mid-grey.
    use manifold_ui_paint::clip_thumb_gpu::create_box_downsample_pipeline;

    let device = GpuDevice::new_queued("ui_color_swatches");
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
    let i = ((32 * 64 + 32) * FORMAT.bytes_per_pixel()) as usize;
    let r = (half::f16::from_bits(u16::from_le_bytes([bytes[i], bytes[i + 1]])).to_f32() * 255.0).round() as u8;
    assert!(
        (64..=192).contains(&r),
        "box downsample of a checkerboard should read mid-grey, got {r}"
    );
}

/// Texture → CPU bytes, same pattern as the headless spike / parity harness.
fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    readback_w(device, texture, W, H)
}

/// Width-parameterized readback (the transport demo renders at 1920 wide).
/// `width * 8` must be 256-byte aligned (1920*8 = 15360 = 60*256).
fn readback_w(device: &GpuDevice, texture: &GpuTexture, width: u32, height: u32) -> Vec<u8> {
    let bytes_per_row = width * FORMAT.bytes_per_pixel();
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

fn save_capture(path: &str, bytes: &[u8], width: u32, height: u32) {
    let capture = LinearUiReadback::from_bytes(
        bytes,
        width,
        height,
        FORMAT,
        AlphaInterpretation::PremultipliedOverBlack,
    )
    .unwrap_or_else(|e| panic!("prepare {path}: {e}"));
    capture
        .to_srgb_rgba8()
        .write_png(Path::new(path))
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}


/// PRESET_BROWSER_AUDITION P1 demo artifact (L2): renders BOTH preset
/// browsers from the REAL registry — the actual post-recuration item set,
/// categories, factory thumbnails (with the deleted placeholders now falling
/// back to text cells), and the generator browser's video-layer view (the
/// LED-* presets gated out per D8). Unlike `browser_popup_demo` above, which
/// uses synthetic names to check the container, this is the choosing surface
/// a performer would see. Item construction mirrors
/// `manifold-app/src/ui_root/dropdowns.rs::build_preset_picker_items`.
#[test]
fn browser_popup_real_registry_p1_demo() {
    use manifold_core::preset_def::PresetKind;
    use manifold_core::preset_type_registry;
    use manifold_ui::node::Vec2;
    use manifold_ui::panels::browser_popup::{
        BrowserPopupMode, BrowserPopupPanel, BrowserPopupRequest,
    };
    use manifold_ui::panels::picker_core::{PickerItem, Source};
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

    let device = GpuDevice::new_queued("ui_color_swatches");
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());

    fn items_for(kind: PresetKind) -> Vec<PickerItem> {
        let mut items: Vec<PickerItem> = preset_type_registry::available_of_kind(kind)
            .iter()
            .map(|reg| PickerItem {
                label: reg.display_name.to_string(),
                type_id: reg.id.as_str().to_string(),
                category: reg.category.map(|c| c.to_string()),
                search_text: None,
                source: Some(Source::Factory),
                thumbnail: manifold_compositor::preset_thumbnail::factory_thumbnail_path(
                    kind,
                    reg.id.as_str(),
                )
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned()),
            })
            .collect();
        items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        items
    }

    // Category chips the way the app builds them: effect mode keeps the
    // fixed ALL_CATEGORIES order (minus buckets with no items post-D9 —
    // Diagnostic is gone), generator mode derives from the items.
    fn chip_names(items: &[PickerItem], fixed: Option<&[&str]>) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        if let Some(buckets) = fixed {
            for b in buckets {
                if items.iter().any(|it| it.category.as_deref() == Some(b)) {
                    names.push((*b).to_string());
                }
            }
        }
        for it in items {
            if let Some(c) = it.category.as_deref()
                && !names.iter().any(|n| n == c)
            {
                names.push(c.to_string());
            }
        }
        names
    }

    // The generator view is rendered for a non-DMX (video/generator) layer —
    // D8's layer_types gate must keep every LED-* preset out of this list.
    let effect_items = items_for(PresetKind::Effect);
    let generator_items: Vec<PickerItem> = preset_type_registry::available_of_kind(PresetKind::Generator)
        .iter()
        .filter(|reg| reg.layer_types.is_none())
        .map(|reg| PickerItem {
            label: reg.display_name.to_string(),
            type_id: reg.id.as_str().to_string(),
            category: reg.category.map(|c| c.to_string()),
            search_text: None,
            source: Some(Source::Factory),
            thumbnail: manifold_compositor::preset_thumbnail::factory_thumbnail_path(
                PresetKind::Generator,
                reg.id.as_str(),
            )
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned()),
        })
        .collect();
    let mut generator_items = generator_items;
    generator_items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    assert!(
        generator_items.iter().all(|it| !it.type_id.starts_with("LED ")),
        "D8 gate: LED presets must not appear in the non-DMX generator browser"
    );

    let cases = [
        (
            "p1_effects_browser.png",
            BrowserPopupRequest {
                mode: BrowserPopupMode::Effect,
                tab: manifold_ui::panels::InspectorTab::Master,
                layer_id: None,
                items: effect_items.clone(),
                category_names: chip_names(&effect_items, Some(preset_type_registry::ALL_CATEGORIES)),
                spawn_graph_pos: None,
                paste_count: 0,
                screen_anchor: Vec2::new(30.0, 40.0),
            },
        ),
        (
            "p1_generators_browser.png",
            BrowserPopupRequest {
                mode: BrowserPopupMode::Generator,
                tab: manifold_ui::panels::InspectorTab::Layer,
                layer_id: None,
                items: generator_items.clone(),
                category_names: chip_names(&generator_items, None),
                spawn_graph_pos: None,
                paste_count: 0,
                screen_anchor: Vec2::new(30.0, 40.0),
            },
        ),
    ];

    for (filename, request) in cases {
        let png = format!("{out_dir}/{filename}");
        let mut popup = BrowserPopupPanel::new();
        popup.set_screen_size(W as f32, H as f32);
        popup.open(request);

        let mut tree = UITree::new();
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
        assert!(drew, "browser popup produced no draw commands");
        let target = RenderTarget::new(&device, W, H, FORMAT, "browser-popup-p1-demo");
        {
            let mut enc = device.create_encoder("browser-popup-p1-render");
            ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
            enc.commit_and_wait_completed();
        }
        let bytes = readback(&device, &target.texture);
        save_capture(&png, &bytes, W, H);
        eprintln!("browser popup P1 demo → {png}");
    }
}

/// PRESET_BROWSER_AUDITION P3 demo artifact (L2): the real-registry browsers
/// on the P3 layout — content-sized width (8 columns of 16:9 cells at this
/// 1080p-class canvas), caption strip with the label row inside it, measured
/// and wrapped chips, the new accent table, and the "No presets match" empty
/// state (third PNG, a search that filters everything out). Renders at
/// 1920×1080 because the P3 popup is wider than the standard swatches canvas.
#[test]
fn browser_popup_real_registry_p3_demo() {
    use manifold_core::preset_def::PresetKind;
    use manifold_core::preset_type_registry;
    use manifold_ui::node::Vec2;
    use manifold_ui::panels::browser_popup::{
        BrowserPopupMode, BrowserPopupPanel, BrowserPopupRequest,
    };
    use manifold_ui::panels::picker_core::{PickerItem, Source};
    use manifold_ui::{Rect, UIFlags, UITree, ZTier};

    const PW: u32 = 1920;
    const PH: u32 = 1080;

    let device = GpuDevice::new_queued("ui_color_swatches");
    let mut ui = UIRenderer::new(&device, FORMAT);
    let out_dir = std::env::var("SWATCH_OUT").unwrap_or_else(|_| "/tmp".to_string());

    fn items_for(kind: PresetKind) -> Vec<PickerItem> {
        let mut items: Vec<PickerItem> = preset_type_registry::available_of_kind(kind)
            .iter()
            .map(|reg| PickerItem {
                label: reg.display_name.to_string(),
                type_id: reg.id.as_str().to_string(),
                category: reg.category.map(|c| c.to_string()),
                search_text: None,
                source: Some(Source::Factory),
                thumbnail: manifold_compositor::preset_thumbnail::factory_thumbnail_path(
                    kind,
                    reg.id.as_str(),
                )
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().into_owned()),
            })
            .collect();
        items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        items
    }

    let effect_items = items_for(PresetKind::Effect);
    let mut generator_items: Vec<PickerItem> = preset_type_registry::available_of_kind(
        PresetKind::Generator,
    )
    .iter()
    .filter(|reg| reg.layer_types.is_none())
    .map(|reg| PickerItem {
        label: reg.display_name.to_string(),
        type_id: reg.id.as_str().to_string(),
        category: reg.category.map(|c| c.to_string()),
        search_text: None,
        source: Some(Source::Factory),
        thumbnail: manifold_compositor::preset_thumbnail::factory_thumbnail_path(
            PresetKind::Generator,
            reg.id.as_str(),
        )
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned()),
    })
    .collect();
    generator_items.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));

    // Decode + register every distinct thumbnail up front, exactly as the
    // app's per-frame thumbnail pass does — without registration the image
    // cells fall back to flat cells and the P3 caption-strip layout (the
    // thing this demo exists to show) never renders.
    let mut registered: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for path in effect_items
        .iter()
        .chain(generator_items.iter())
        .filter_map(|it| it.thumbnail.as_deref())
    {
        let handle = manifold_ui::node::texture_handle_for_key(path);
        if !registered.insert(handle) {
            continue;
        }
        let (tw, th, rgba) =
            manifold_compositor::preset_thumbnail::decode_png_rgba8(std::path::Path::new(path))
                .expect("decode committed factory thumbnail");
        assert!(
            ui.register_image(&device, handle, tw, th, &rgba),
            "thumbnail must register into the UIRenderer image cache"
        );
    }

    // Category chips the way the app builds them after P3: only buckets
    // that hold items (D9 — Diagnostic leaves the filter row), generators
    // derived from the items.
    let effect_chips: Vec<String> = preset_type_registry::ALL_CATEGORIES
        .iter()
        .filter(|c| effect_items.iter().any(|it| it.category.as_deref() == Some(c)))
        .map(|&c| c.to_string())
        .collect();
    let mut gen_chips: Vec<String> = Vec::new();
    for it in &generator_items {
        if let Some(c) = it.category.as_deref()
            && !gen_chips.iter().any(|n| n == c)
        {
            gen_chips.push(c.to_string());
        }
    }

    struct Case {
        filename: &'static str,
        mode: BrowserPopupMode,
        items: Vec<PickerItem>,
        chips: Vec<String>,
        filter: &'static str,
    }
    let cases = [
        Case {
            filename: "p3_effects_browser.png",
            mode: BrowserPopupMode::Effect,
            items: effect_items,
            chips: effect_chips.clone(),
            filter: "",
        },
        Case {
            filename: "p3_generators_browser.png",
            mode: BrowserPopupMode::Generator,
            items: generator_items,
            chips: gen_chips,
            filter: "",
        },
        Case {
            filename: "p3_empty_state.png",
            mode: BrowserPopupMode::Effect,
            items: items_for(PresetKind::Effect),
            chips: effect_chips,
            filter: "qqq-no-such-preset",
        },
    ];

    for case in cases {
        let png = format!("{out_dir}/{}", case.filename);
        let mut popup = BrowserPopupPanel::new();
        popup.set_screen_size(PW as f32, PH as f32);
        popup.open(BrowserPopupRequest {
            mode: case.mode,
            tab: manifold_ui::panels::InspectorTab::Master,
            layer_id: None,
            items: case.items,
            category_names: case.chips,
            spawn_graph_pos: None,
            paste_count: 0,
            screen_anchor: Vec2::new(60.0, 70.0),
        });
        if !case.filter.is_empty() {
            popup.set_filter(case.filter.to_string());
        }

        let mut tree = UITree::new();
        // A CoreText-accurate measurer is what the app installs; the
        // heuristic default the bare tree carries is close enough for a
        // demo, so no measurer is wired here (same as the P1/P2 demos).
        let region = tree.begin_region(
            Rect::new(0.0, 0.0, PW as f32, PH as f32),
            ZTier::Overlay,
            "browser_popup",
            UIFlags::empty(),
        );
        let start = tree.count();
        popup.build(&mut tree);
        tree.end_region(region, start);

        ui.begin_frame();
        ui.draw_rect(0.0, 0.0, PW as f32, PH as f32, color::BG_3);
        ui.render_tree(&tree, None);
        let drew = ui.prepare(&device, PW, PH, 1.0);
        assert!(drew, "browser popup produced no draw commands");
        let target = RenderTarget::new(&device, PW, PH, FORMAT, "browser-popup-p3-demo");
        {
            let mut enc = device.create_encoder("browser-popup-p3-render");
            ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
            enc.commit_and_wait_completed();
        }
        let bytes = readback_w(&device, &target.texture, PW, PH);
        save_capture(&png, &bytes, PW, PH);
        eprintln!("browser popup P3 demo → {png}");
    }
}
