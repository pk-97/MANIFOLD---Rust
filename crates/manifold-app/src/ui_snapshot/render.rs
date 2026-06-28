//! Windowless render of a built `UIRoot` to a PNG. Mirrors the proven headless
//! pattern in `manifold-renderer/tests/...` (`GpuDevice::new()` has no window),
//! plus the clip passes from `app_render`: clip bodies, optional injected
//! thumbnails, and clip names are immediate-mode passes (NOT `UITree` nodes),
//! drawn on top of the tree in order (bodies → thumbs → names) with `Load`.
//! See `docs/HEADLESS_UI_HARNESS.md`.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::clip_draw::{emit_clip_names, emit_clips, ClipBody};
use manifold_renderer::clip_thumb_gpu::ClipThumbGpu;
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;

use super::thumbs;
use crate::ui_root::UIRoot;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

/// Render the whole UI (`ui.tree` + clip bodies + optional injected thumbnails +
/// clip names) into a `tex_w`×`tex_h` texture and save as PNG. `tex_w` must be a
/// multiple of 64 so the readback stride (`tex_w * 4`) is 256-byte aligned.
pub fn render_ui_to_png(
    ui: &UIRoot,
    selection: &manifold_ui::UIState,
    tex_w: u32,
    tex_h: u32,
    scale: f32,
    with_thumbs: bool,
    path: &str,
) {
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

    // Clips are immediate-mode passes, not tree nodes (same emit path as
    // app_render 4b/5). Resolve the visible clips once, reused across passes.
    let mut clip_rects = Vec::new();
    ui.viewport.visible_clip_rects(&mut clip_rects);
    if !clip_rects.is_empty() {
        let tracks = ui.viewport.get_tracks_rect();

        // Pass 2: GPU clip bodies (Load).
        // Resolve per-clip selected/hovered from real state, exactly as
        // app_render does — never pin them false (a hardcode would misrepresent
        // clip selection once a `select:clip` scene exists).
        let hovered_clip = ui.viewport.hovered_clip_id();
        let bodies: Vec<ClipBody> = clip_rects
            .iter()
            .map(|cr| ClipBody {
                rect: cr.rect,
                base_color: cr.base_color,
                selected: selection.is_selected(&cr.clip_id),
                hovered: hovered_clip == Some(cr.clip_id.as_str()),
                muted: cr.is_muted,
                locked: cr.is_locked,
                generator: cr.is_generator,
            })
            .collect();
        renderer.begin_frame();
        renderer.push_immediate_clip(tracks.x, tracks.y, tracks.width, tracks.height);
        emit_clips(&mut renderer, &bodies);
        renderer.pop_immediate_clip();
        if renderer.prepare(&device, tex_w, tex_h, dpi) {
            let mut enc = device.create_encoder("ui-snap-clips");
            renderer.render(&mut enc, &target.texture, GpuLoadAction::Load);
            enc.commit_and_wait_completed();
        }

        // Pass 3: injected test thumbnails (Load), through the real ClipThumbGpu.
        if with_thumbs {
            let atlas = thumbs::make_test_atlas(&device);
            let quads = thumbs::build_quads(&clip_rects);
            if !quads.is_empty() {
                let mut thumb = ClipThumbGpu::new(&device, FORMAT);
                let mut enc = device.create_encoder("ui-snap-thumbs");
                thumb.render(
                    &device,
                    &mut enc,
                    &target.texture,
                    tex_w,
                    tex_h,
                    scale,
                    tracks,
                    &atlas,
                    &quads,
                );
                enc.commit_and_wait_completed();
            }
        }

        // Pass 4: clip names on top (Load).
        renderer.begin_frame();
        emit_clip_names(&mut renderer, &clip_rects, tracks);
        if renderer.prepare(&device, tex_w, tex_h, dpi) {
            let mut enc = device.create_encoder("ui-snap-names");
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
