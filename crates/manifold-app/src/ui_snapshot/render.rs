//! Windowless render of a built `UIRoot` to a PNG. Mirrors the proven headless
//! pattern in `manifold-renderer/tests/headless_ui_spike.rs` (`GpuDevice::new()`
//! has no window), plus the clip pass from `app_render` Pass 4b/5: clip bodies
//! and names are an immediate-mode pass, NOT `UITree` nodes, so they are emitted
//! in a second prepare/render cycle drawn on top of the tree.
//! See `docs/HEADLESS_UI_HARNESS.md`.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::clip_draw::{emit_clip_names, emit_clips, ClipBody};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;

use crate::ui_root::UIRoot;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

/// Render the whole UI (`ui.tree` + the clip bodies/names) into a `tex_w`×`tex_h`
/// texture and save as PNG. `tex_w` must be a multiple of 64 so the readback
/// stride (`tex_w * 4`) is 256-byte aligned.
pub fn render_ui_to_png(ui: &UIRoot, tex_w: u32, tex_h: u32, scale: f32, path: &str) {
    assert_eq!(tex_w % 64, 0, "tex_w must be a multiple of 64 for aligned readback");

    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap");
    let dpi = f64::from(scale);

    // Pass 1: the UITree — headers, ruler, lane backgrounds, playhead, markers.
    renderer.begin_frame();
    renderer.render_tree(&ui.tree, None);
    let drew = renderer.prepare(&device, tex_w, tex_h, dpi);
    {
        let mut enc = device.create_encoder("ui-snap-tree");
        renderer.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "prepare() reported no UI content to draw");

    // Pass 2: GPU clip bodies + names, drawn on top (Load). Clips are an
    // immediate-mode pass, not tree nodes — same emit path as app_render 4b/5.
    let mut clip_rects = Vec::new();
    ui.viewport.visible_clip_rects(&mut clip_rects);
    if !clip_rects.is_empty() {
        let bodies: Vec<ClipBody> = clip_rects
            .iter()
            .map(|cr| ClipBody {
                rect: cr.rect,
                base_color: cr.base_color,
                selected: false,
                hovered: false,
                muted: cr.is_muted,
                locked: cr.is_locked,
                generator: cr.is_generator,
            })
            .collect();
        let tracks = ui.viewport.get_tracks_rect();

        renderer.begin_frame();
        renderer.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
        emit_clips(&mut renderer, &bodies);
        renderer.pop_immediate_clip();
        emit_clip_names(&mut renderer, &clip_rects);
        if renderer.prepare(&device, tex_w, tex_h, dpi) {
            let mut enc = device.create_encoder("ui-snap-clips");
            renderer.render(&mut enc, &target.texture, GpuLoadAction::Load);
            enc.commit_and_wait_completed();
        }
    }

    let bytes = readback(&device, &target.texture, tex_w, tex_h);
    image::save_buffer(path, &bytes, tex_w, tex_h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {path}: {e}"));
}

fn readback(device: &GpuDevice, texture: &GpuTexture, w: u32, h: u32) -> Vec<u8> {
    let bytes_per_row = w * 4;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);

    let mut enc = device.create_encoder("ui-snap-readback");
    enc.copy_texture_to_buffer(texture, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer is mapped");
    let bytes: &[u8] =
        unsafe { slice::from_raw_parts(ptr.cast::<c_void>().cast::<u8>(), total as usize) };
    bytes.to_vec()
}
