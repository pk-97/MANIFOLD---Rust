//! Native rendering proof for automation strokes: coverage at subpixel edges
//! and an uninterrupted centre at both desktop scale factors.
#![cfg(all(target_os = "macos", feature = "gpu-proofs"))]

use manifold_gpu::{GpuDevice, GpuLoadAction, GpuTextureFormat};
use manifold_renderer::{render_target::RenderTarget, ui_renderer::UIRenderer};
use manifold_ui::node::Color32;

#[test]
fn automation_stroke_has_antialiased_edges_and_continuous_centre() {
    let device = GpuDevice::new();
    let format = GpuTextureFormat::Rgba8Unorm;
    let mut ui = UIRenderer::new(&device, format);
    for scale in [1.0_f32, 2.0] {
        let width = (256.0 * scale) as u32;
        let height = (128.0 * scale) as u32;
        ui.begin_frame();
        ui.draw_rect(0.0, 0.0, 256.0, 128.0, Color32::new(0, 0, 0, 255));
        ui.draw_aa_line(24.0, 24.0, 220.0, 76.0, 2.0, Color32::new(255, 255, 255, 255));
        ui.draw_grid_line(8.25, 90.0, 8.25, 110.0, manifold_ui::bitmap_renderer::TimingGridLineKind::Bar);
        ui.draw_grid_line(12.25, 90.0, 12.25, 110.0, manifold_ui::bitmap_renderer::TimingGridLineKind::Beat);
        // Renderer coordinates and viewport dimensions are logical pixels;
        // only the target/readback dimensions scale to physical pixels.
        assert!(ui.prepare(&device, 256, 128, f64::from(scale)));
        let target = RenderTarget::new(&device, width, height, format, "automation-stroke");
        let mut encoder = device.create_encoder("automation-stroke-render");
        ui.render(&mut encoder, &target.texture, GpuLoadAction::Clear);
        encoder.commit_and_wait_completed();

        let bytes_per_row = width * 4;
        let size = (height * bytes_per_row) as usize;
        let buffer = device.create_buffer_shared(size as u64);
        let mut encoder = device.create_encoder("automation-stroke-readback");
        encoder.copy_texture_to_buffer(&target.texture, &buffer, width, height, bytes_per_row);
        encoder.commit_and_wait_completed();
        let ptr = buffer.mapped_ptr().expect("shared buffer is mapped");
        // The completed readback owns `size` bytes for the lifetime of buffer.
        let pixels = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) };
        let partial = pixels.chunks_exact(4).filter(|p| p[0] > 8 && p[0] < 247).count();
        assert!(partial > 100, "scale {scale}: edge pixels need partial coverage, got {partial}");
        for sample in 1..100 {
            let t = sample as f32 / 100.0;
            let x = ((24.0 + 196.0 * t) * scale) as usize;
            let y = ((24.0 + 52.0 * t) * scale) as usize;
            let red = pixels[(y * width as usize + x) * 4];
            assert!(red > 100, "scale {scale}: gap at sample {sample}: {red}");
        }
        let row = (100.0 * scale) as usize;
        let columns: Vec<_> = (0..width as usize)
            .filter(|&x| pixels[(row * width as usize + x) * 4] > 0).collect();
        let bar = (8.25 * scale).round() as usize;
        let beat = (12.25 * scale).round() as usize;
        assert_eq!(columns, vec![bar, bar + 1, beat], "grid widths must stay 2/1 physical pixels at {scale}x");
        let output = std::env::temp_dir().join(format!("manifold-automation-stroke-{}x.png", scale as u32));
        image::save_buffer(&output, pixels, width, height, image::ExtendedColorType::Rgba8).unwrap();
    }
}
