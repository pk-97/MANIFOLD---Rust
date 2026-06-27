//! Headless render of the real `LayerHeaderPanel` → PNG, for eyeballing the
//! timeline-UI redesign against `scratchpad/timeline-mockup.html`.
//!
//! Reuses the windowless harness from `headless_ui_spike.rs`: `GpuDevice::new()`
//! has no winit window, `UIRenderer` rasterizes a `UITree`, we read the texture
//! back and save a PNG. Run with:
//!   cargo test -p manifold-renderer --test timeline_header_preview -- --nocapture
//! then open the PNG path it prints.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::{Color32, LayerHeaderPanel, LayerInfo, Panel, ScreenLayout, UITree};

const W: u32 = 256; // 256*4 = 1024 = 4*256 → aligned readback stride; captures the 200px panel
const H: u32 = 1100;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

const OUT: &str = "/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/b8ef8a0e-3fc6-4363-a34a-8297f971a196/scratchpad/native_header_baseline.png";

fn layer(name: &str, y: f32, h: f32, color: Color32) -> LayerInfo {
    LayerInfo {
        name: name.into(),
        layer_id: name.into(),
        is_collapsed: false,
        is_group: false,
        is_generator: false,
        is_audio: false,
        is_muted: false,
        is_solo: false,
        analysis_only: false,
        is_led: false,
        parent_layer_id: None,
        blend_mode: "Normal".into(),
        generator_type: None,
        clip_count: 5,
        video_folder_path: None,
        source_clip_count: 0,
        midi_note: -1,
        midi_channel: -1,
        midi_device: None,
        midi_all_notes: false,
        audio_gain_db: 0.0,
        audio_send_name: None,
        y_offset: y,
        height: h,
        is_selected: false,
        color,
    }
}

#[test]
fn render_timeline_header_baseline() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    // Mockup-like stack: text(pink) / video(blue) / generator(purple, SELECTED,
    // expanded) / group(slate) + child(green) / audio(gold).
    let pink = Color32::new(194, 85, 127, 255);
    let blue = Color32::new(100, 148, 210, 255);
    let purple = Color32::new(125, 80, 200, 255);
    let slate = Color32::new(98, 104, 124, 255);
    let green = Color32::new(63, 154, 100, 255);
    let gold = Color32::new(194, 135, 63, 255);

    let mut text = layer("TEXT BOT L", 0.0, 140.0, pink);
    text.is_muted = true;

    let flowers = layer("FLOWERS", 140.0, 140.0, blue);

    let mut plasma = layer("PLASMA", 280.0, 200.0, purple);
    plasma.is_generator = true;
    plasma.generator_type = Some("Plasma".into());
    plasma.is_solo = true;
    plasma.is_selected = true;
    plasma.midi_note = 60;
    plasma.midi_channel = 1;

    let mut group = layer("BG STACK", 480.0, 70.0, slate);
    group.is_group = true;

    let mut clouds = layer("CLOUDS", 550.0, 140.0, green);
    clouds.parent_layer_id = Some("BG STACK".into());

    let mut kick = layer("KICK", 690.0, 140.0, gold);
    kick.is_audio = true;
    kick.audio_send_name = Some("Drums".into());

    let layers = vec![text, flowers, plasma, group, clouds, kick];

    // Push the timeline to fill most of the screen so the layer-controls panel
    // starts near the top and the rows land inside the captured texture.
    let mut layout = ScreenLayout::new(1920.0, 1100.0);
    layout.timeline_split_ratio = 0.96;
    let mut panel = LayerHeaderPanel::new();
    panel.set_layers(layers);
    let mut tree = UITree::new();
    panel.build(&mut tree, &layout);

    eprintln!("lc = {:?}", layout.layer_controls());
    eprintln!("track_header_height = {}", layout.track_header_height());
    for i in 0..panel.layer_count() {
        eprintln!(
            "row {i} name bounds = {:?}",
            panel.get_node_bounds(&tree, panel.name_node_id(i))
        );
    }

    render_to_png(&device, &mut ui, &tree, OUT);
    eprintln!("native header baseline → {OUT}");
}

fn render_to_png(device: &GpuDevice, ui: &mut UIRenderer, tree: &UITree, path: &str) -> Vec<u8> {
    let target = RenderTarget::new(device, W, H, FORMAT, "header-preview");
    ui.begin_frame();
    ui.render_tree(tree, None);
    let drew = ui.prepare(device, W, H, 1.0);
    {
        let mut enc = device.create_encoder("header-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "prepare() reported no UI content to draw");

    let bytes = readback(device, &target.texture);
    image::save_buffer(path, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
    bytes
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("header-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
