//! Windowless render of a built `UITree` to a PNG. Mirrors the proven headless
//! pattern in `manifold-renderer/tests/headless_ui_spike.rs`: `GpuDevice::new()`
//! has no winit window, `UIRenderer` rasterizes the tree, we read the texture
//! back and save a PNG. See `docs/HEADLESS_UI_HARNESS.md`.

use std::ffi::c_void;
use std::slice;

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTexture, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::UITree;

const FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

/// Render `tree` into a `tex_w`×`tex_h` texture at `scale` DPI and save as PNG.
///
/// `tex_w` must be a multiple of 64 so the readback stride (`tex_w * 4`) is
/// 256-byte aligned (same constraint the existing harnesses document).
pub fn render_tree_to_png(tree: &UITree, tex_w: u32, tex_h: u32, scale: f32, path: &str) {
    assert_eq!(tex_w % 64, 0, "tex_w must be a multiple of 64 for aligned readback");

    let device = GpuDevice::new();
    let mut renderer = UIRenderer::new(&device, FORMAT);
    let target = RenderTarget::new(&device, tex_w, tex_h, FORMAT, "ui-snap");

    renderer.begin_frame();
    renderer.render_tree(tree, None);
    let drew = renderer.prepare(&device, tex_w, tex_h, f64::from(scale));
    {
        let mut enc = device.create_encoder("ui-snap-render");
        renderer.render(&mut enc, &target.texture, GpuLoadAction::Clear);
        enc.commit_and_wait_completed();
    }
    assert!(drew, "prepare() reported no UI content to draw");

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
