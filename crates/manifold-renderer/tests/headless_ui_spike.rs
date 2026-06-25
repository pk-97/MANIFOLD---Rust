//! Phase -1 spike — prove the headless UI render + interaction harness.
//!
//! Goal (docs/UI_DESIGN_SYSTEM_AND_INSPECTOR_REDESIGN.md §23): with NO winit
//! Window, build an effect card, render it to a PNG, drive a synthetic click on
//! its collapse chevron, apply the toggle the way the app bridge does, re-render,
//! and verify the two frames differ (the param rows appeared).
//!
//! Reuses the known-good headless infra: `GpuDevice::new()` is windowless,
//! `UIRenderer` rasterizes a `UITree` to a `GpuTexture`, and we read the texture
//! back to CPU exactly like the parity harness does, then save a PNG.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;

use manifold_foundation::EffectId;
use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::{
    PanelAction, ParamCardConfig, ParamCardKind, ParamCardPanel, ParamInfo, PointerAction, Rect,
    UIEvent, UIInputSystem, UITree, Vec2,
};

// 256-byte-aligned row stride (320 * 4 = 1280 = 5 * 256) keeps the texture→buffer
// readback copy happy.
const W: u32 = 320;
const H: u32 = 256;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;
const CARD: Rect = Rect {
    x: 20.0,
    y: 20.0,
    width: 280.0,
    height: 220.0,
};

#[test]
fn headless_render_and_chevron_click() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    let out_dir = std::env::var("SPIKE_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let collapsed_png = format!("{out_dir}/spike_collapsed.png");
    let expanded_png = format!("{out_dir}/spike_expanded.png");

    // ── Build the card collapsed, render PNG #1 ──────────────────────────
    let mut panel = ParamCardPanel::new();
    panel.configure(&effect_config(true)); // collapsed
    let mut tree = UITree::new();
    panel.build(&mut tree, CARD);
    let collapsed_bytes = render_to_png(&device, &mut ui, &tree, &collapsed_png);

    // ── Synthetic click on the chevron, no winit ─────────────────────────
    let chevron = panel
        .chevron_node_id()
        .expect("chevron node id resolved after build");
    let cb = tree.get_bounds(chevron);
    let center = Vec2::new(cb.x + cb.width * 0.5, cb.y + cb.height * 0.5);

    let mut input = UIInputSystem::new();
    input.process_pointer(&mut tree, center, PointerAction::Down, 0.0);
    input.process_pointer(&mut tree, center, PointerAction::Up, 0.0);

    let clicked = input
        .drain_events()
        .into_iter()
        .find_map(|e| match e {
            UIEvent::Click { node_id, .. } => Some(node_id),
            _ => None,
        })
        .expect("Down+Up at the chevron center emits a Click");
    assert_eq!(clicked, chevron, "the click resolved to the chevron node");

    let actions = panel.handle_click(clicked);
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, PanelAction::EffectCollapseToggle(0))),
        "chevron click returns EffectCollapseToggle(0); got {actions:?}",
    );

    // The panel does NOT self-toggle — the app bridge writes fx.collapsed and the
    // card re-reads it on the next configure. Apply that one write headlessly.
    panel.set_collapsed(false);

    // ── Re-render expanded, PNG #2 ───────────────────────────────────────
    let mut tree2 = UITree::new();
    panel.build(&mut tree2, CARD);
    let expanded_bytes = render_to_png(&device, &mut ui, &tree2, &expanded_png);

    // ── Verify the drawer expanded: the two frames must differ ───────────
    assert_eq!(collapsed_bytes.len(), expanded_bytes.len());
    let differing = collapsed_bytes
        .iter()
        .zip(expanded_bytes.iter())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing > 2000,
        "expanded card should differ from collapsed by the two param rows; only {differing} bytes differ",
    );

    eprintln!("collapsed → {collapsed_png}");
    eprintln!("expanded  → {expanded_png}  ({differing} bytes changed)");
}

/// Build a complete effect-card config (mirrors the in-crate `effect_config`
/// fixture; every field is public).
fn effect_config(collapsed: bool) -> ParamCardConfig {
    let n = 2;
    ParamCardConfig {
        kind: ParamCardKind::Effect,
        effect_index: 0,
        effect_id: EffectId::new("spike-effect-0"),
        name: "Blur".into(),
        enabled: true,
        collapsed,
        supports_envelopes: true,
        string_params: Vec::new(),
        layer_id: None,
        params: vec![
            param("radius", "Radius", 0.0, 100.0, 10.0),
            param("strength", "Strength", 0.0, 1.0, 0.5),
        ],
        has_drv: false,
        has_env: false,
        has_abl: false,
        has_graph_mod: false,
        driver_active: vec![false; n],
        envelope_active: vec![false; n],
        trim_min: vec![0.0; n],
        trim_max: vec![1.0; n],
        target_norm: vec![1.0; n],
        env_decay: vec![1.0; n],
        driver_beat_div_idx: vec![-1; n],
        driver_waveform_idx: vec![-1; n],
        driver_reversed: vec![false; n],
        driver_dotted: vec![false; n],
        driver_triplet: vec![false; n],
        driver_free_period: vec![None; n],
        audio: Default::default(),
    }
}

fn param(id: &'static str, name: &str, min: f32, max: f32, default: f32) -> ParamInfo {
    ParamInfo {
        param_id: std::borrow::Cow::Borrowed(id),
        name: name.into(),
        min,
        max,
        default,
        whole_numbers: false,
        is_angle: false,
        exposed: true,
        is_toggle: false,
        is_trigger: false,
        value_labels: None,
        osc_address: None,
        ableton_display: None,
        ableton_range: None,
        mappable: false,
    }
}

/// Render a built tree to the target, read it back, save a PNG, return RGBA bytes.
fn render_to_png(device: &GpuDevice, ui: &mut UIRenderer, tree: &UITree, path: &str) -> Vec<u8> {
    let target = RenderTarget::new(device, W, H, FORMAT, "spike-ui");
    ui.begin_frame();
    ui.render_tree(tree, None);
    let drew = ui.prepare(device, W, H, 1.0);
    {
        let mut enc = device.create_encoder("spike-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "prepare() reported no UI content to draw");

    let bytes = readback(device, &target.texture);
    image::save_buffer(path, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
    bytes
}

/// Texture → CPU bytes, same pattern as `tests/parity/harness.rs::readback`.
fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("spike-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
