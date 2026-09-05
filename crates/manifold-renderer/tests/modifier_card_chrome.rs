//! Modifier-card chrome contact sheet (BUG-oe99) — renders a regular effect
//! card next to a Scene Loop modifier card into one PNG so the header chrome
//! parity (cog, ON, DBG, ×, chevron) and the section-header-stripped body can
//! be eyeballed headlessly. Same windowless render path as
//! `ui_color_swatches.rs`: `GpuDevice::new()` → `UIRenderer::render_tree` →
//! texture readback → PNG.
//!
//! Run: `MOD_CHROME_OUT=/some/dir cargo test -p manifold-renderer --test modifier_card_chrome`

#![cfg(target_os = "macos")]

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::param_surface::{
    ModifierCardInfo, ParamRow, ParamSurface, RowMapping, RowSpec, RowValue, SceneRowAddr,
};
use manifold_ui::panels::param_card::{ParamCardKind, ParamCardPanel, RowMod};
use manifold_ui::{Rect, UITree};

// W*4 must be 256-byte aligned for the texture→buffer readback copy.
// 640*4 = 2560 = 10*256.
const W: u32 = 640;
const H: u32 = 420;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

fn row(id: &'static str, name: &str, min: f32, max: f32, base: f32, whole: bool) -> ParamRow {
    ParamRow {
        id: std::borrow::Cow::Borrowed(id),
        spec: RowSpec {
            name: name.into(),
            min,
            max,
            default: base,
            whole_numbers: whole,
            is_angle: false,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            // Modifier surfaces strip the section app-side (the stamped name
            // duplicates the card title) — that post-strip state is what this
            // sheet renders.
            section: None,
        },
        value: RowValue { base, effective: base, exposed: true, driven: false },
        modulation: RowMod::default(),
        mapping: RowMapping {
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
        },
        scene_addr: None,
    }
}

fn effect_surface() -> ParamSurface {
    ParamSurface {
        kind: ParamCardKind::Effect,
        title: "Bloom".into(),
        effect_index: 0,
        effect_id: manifold_foundation::EffectId::new("fx-bloom"),
        enabled: true,
        collapsed: false,
        supports_envelopes: true,
        has_graph_mod: false,
        layer_id: None,
        modifier: None,
        rows: vec![row("amount", "Amount", 0.0, 5.0, 1.2, false)],
        string_params: vec![],
        audio: Default::default(),
        relight: Default::default(),
    }
}

fn modifier_surface() -> ParamSurface {
    ParamSurface {
        kind: ParamCardKind::Effect,
        title: "Scene Loop".into(),
        effect_index: 0,
        effect_id: manifold_foundation::EffectId::new("scene_modifier:scene_loop"),
        enabled: true,
        collapsed: false,
        supports_envelopes: true,
        has_graph_mod: false,
        layer_id: None,
        modifier: Some(ModifierCardInfo {
            kind_id: "scene_loop".into(),
            layer_id: manifold_foundation::LayerId::new("layer-a"),
            show_enable_toggle: true,
            wrap_debug: Some(SceneRowAddr {
                scope_path: Vec::new(),
                node_doc_id: 7,
                param_id: "bars".into(),
            }),
        }),
        rows: vec![
            row("bars", "Bars", 1.0, 64.0, 8.0, true),
            row("copies", "Copies", 1.0, 8.0, 8.0, true),
            row("spacing", "Spacing", 0.25, 16.0, 4.0, false),
        ],
        string_params: vec![],
        audio: Default::default(),
        relight: Default::default(),
    }
}

#[test]
fn modifier_card_chrome_contact_sheet() {
    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target = RenderTarget::new(&device, W, H, FORMAT, "modifier-chrome");

    let out_dir = std::env::var("MOD_CHROME_OUT")
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    let png = format!("{out_dir}/modifier_card_chrome.png");

    let mut tree = UITree::new();
    let region = tree.begin_region(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        manifold_ui::ZTier::Base,
        "modifier_chrome",
        manifold_ui::UIFlags::empty(),
    );
    let start = tree.count();
    tree.add_panel(
        None,
        0.0,
        0.0,
        W as f32,
        H as f32,
        manifold_ui::node::UIStyle {
            bg_color: manifold_ui::color::BG_1,
            ..Default::default()
        },
    );

    let mut effect = ParamCardPanel::new();
    effect.configure(&effect_surface());
    effect.build(&mut tree, Rect::new(16.0, 16.0, 288.0, 380.0));

    let mut modifier = ParamCardPanel::new();
    modifier.configure(&modifier_surface());
    modifier.build(&mut tree, Rect::new(336.0, 16.0, 288.0, 380.0));

    tree.end_region(region, start);

    renderer.begin_frame();
    renderer.render_tree(&tree, None);
    let drew = renderer.prepare(&device, W, H, 1.0);
    {
        let mut enc = device.create_encoder("modifier-chrome");
        renderer.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "modifier chrome sheet produced no draw commands");

    let bytes = readback(&device, &target.texture);
    image::save_buffer(&png, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {png}: {e}"));
    eprintln!("modifier chrome contact sheet → {png}");
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("modifier-chrome-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), total as usize) };
    bytes.to_vec()
}
