//! Headless render of GPU clip bodies + name labels → PNG, to verify §E of the
//! timeline redesign (layer-coloured clips, selection border, title-on-bottom)
//! against `scratchpad/timeline-mockup.html`.
//!
//! Same windowless harness as `headless_ui_spike.rs` / `timeline_header_preview.rs`,
//! but driving the immediate clip emitters (`emit_clips` + `emit_clip_names`)
//! instead of a UITree.

#![cfg(target_os = "macos")]

use std::ffi::c_void;
use std::slice;
use std::sync::Arc;

use manifold_foundation::{Beats, ClipId};
use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::clip_draw::{emit_clip_names, emit_clips, ClipBody};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::node::{Color32, Rect};
use manifold_ui::panels::viewport::ClipScreenRect;

const W: u32 = 512; // 512*4 = 2048 = 8*256 → aligned readback stride
const H: u32 = 160;
const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

const OUT: &str = "/private/tmp/claude-501/-Users-peterkiemann-MANIFOLD---Rust/b8ef8a0e-3fc6-4363-a34a-8297f971a196/scratchpad/native_clips.png";

struct Clip {
    rect: Rect,
    color: Color32,
    name: &'static str,
    selected: bool,
    generator: bool,
}

fn body(c: &Clip) -> ClipBody {
    ClipBody {
        rect: c.rect,
        base_color: c.color,
        selected: c.selected,
        hovered: false,
        muted: false,
        locked: false,
        generator: c.generator,
    }
}

fn screen_rect(i: usize, c: &Clip) -> ClipScreenRect {
    ClipScreenRect {
        clip_id: ClipId::new(format!("c{i}")),
        layer_index: i,
        rect: c.rect,
        base_color: c.color,
        name: Arc::from(c.name),
        start_beat: Beats(0.0),
        end_beat: Beats(4.0),
        is_muted: false,
        is_locked: false,
        is_generator: c.generator,
        is_audio: false,
        waveform: None,
        in_point_seconds: 0.0,
        warped_secs_per_beat: 0.5,
    }
}

#[test]
fn render_clip_cards() {
    let device = GpuDevice::new();
    let mut ui = UIRenderer::new(&device, FORMAT);

    // Tall clips so title-bottom is clearly distinct from the old centred label.
    let clips = [
        Clip {
            rect: Rect::new(10.0, 20.0, 150.0, 110.0),
            color: Color32::new(100, 148, 210, 255),
            name: "flowers_loop_A.mov",
            selected: true,
            generator: false,
        },
        Clip {
            rect: Rect::new(170.0, 20.0, 130.0, 110.0),
            color: Color32::new(125, 80, 200, 255),
            name: "Plasma drift",
            selected: false,
            generator: true,
        },
        Clip {
            rect: Rect::new(310.0, 20.0, 190.0, 110.0),
            color: Color32::new(63, 154, 100, 255),
            name: "clouds_slow.mov",
            selected: false,
            generator: false,
        },
    ];

    let bodies: Vec<ClipBody> = clips.iter().map(body).collect();
    let names: Vec<ClipScreenRect> = clips.iter().enumerate().map(|(i, c)| screen_rect(i, c)).collect();

    let target = RenderTarget::new(&device, W, H, FORMAT, "clip-preview");
    ui.begin_frame();
    emit_clips(&mut ui, &bodies);
    emit_clip_names(&mut ui, &names);
    let drew = ui.prepare(&device, W, H, 1.0);
    {
        let mut enc = device.create_encoder("clip-render");
        ui.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "prepare() reported no clip content to draw");

    let bytes = readback(&device, &target.texture);
    image::save_buffer(OUT, &bytes, W, H, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {OUT}: {e}"));
    eprintln!("native clips → {OUT}");
}

fn readback(device: &GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    let bytes_per_row = W * 4;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("clip-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
