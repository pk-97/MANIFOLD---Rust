use half::f16;
use manifold_gpu::{GpuDevice, GpuTexture};

/// Clear `target` to `rgba` and commit the encoder before returning,
/// so the GPU has actually performed the clear by the time the caller
/// uses the texture.
///
/// Test-only convenience: a freshly-created encoder with a
/// `clear_texture` call recorded but never committed silently
/// discards the clear (Metal commands don't execute until commit).
/// Subsequent reads of the texture then see uninitialised
/// (often all-zero) memory, which can pass tests for the wrong
/// reason — black inputs pass against `expected = 0`, white inputs
/// fail noisily. This helper owns the encoder + commit so the bug
/// can't recur.
///
/// Stalls the calling thread until the clear completes; meant for
/// test setup, not hot-path work.
pub(crate) fn clear_texture_committed(
    device: &manifold_gpu::GpuDevice,
    target: &manifold_gpu::GpuTexture,
    rgba: [f64; 4],
    label: &str,
) {
    let mut enc = device.create_encoder(label);
    {
        let mut gpu = crate::gpu::gpu_encoder::GpuEncoder::new(&mut enc, device);
        gpu.clear_texture(target, rgba[0], rgba[1], rgba[2], rgba[3]);
    }
    enc.commit_and_wait_completed();
}

pub fn readback_raw_halves(device: &GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<u8> {
    let bytes_per_row = w * 8;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("headless-convergence-readback");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), total as usize) }.to_vec()
}

pub fn mean_abs_half_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len(), "mean_abs_half_diff: length mismatch");
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for (ca, cb) in a.chunks_exact(2).zip(b.chunks_exact(2)) {
        let va = f16::from_bits(u16::from_le_bytes([ca[0], ca[1]])).to_f32();
        let vb = f16::from_bits(u16::from_le_bytes([cb[0], cb[1]])).to_f32();
        let d = (va - vb).abs();
        if d.is_finite() {
            sum += f64::from(d);
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

pub fn linear_to_srgb8(v: f32) -> u8 {
    let c = v.clamp(0.0, 1.0);
    let s = if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

pub fn readback_srgb_rgba8(
    device: &GpuDevice,
    tex: &GpuTexture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/pixel
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("headless-readback-srgb");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in halves.chunks_exact(4) {
        let r = f16::from_bits(px[0]).to_f32();
        let g = f16::from_bits(px[1]).to_f32();
        let b = f16::from_bits(px[2]).to_f32();
        // Same composite-over-opaque-black as `readback_tonemapped_rgba8`
        // (straight-alpha producers: visible colour is rgb*a over black).
        let a = f16::from_bits(px[3]).to_f32().clamp(0.0, 1.0);
        out.push(linear_to_srgb8(r * a));
        out.push(linear_to_srgb8(g * a));
        out.push(linear_to_srgb8(b * a));
        out.push(255);
    }
    out
}

pub fn encode_rgba8_png(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut bytes: Vec<u8> = Vec::new();
    {
        use image::ImageEncoder;
        let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
        encoder
            .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
            .expect("png encode failed");
    }
    bytes
}

pub fn readback_to_srgb_png_linear(
    device: &GpuDevice,
    texture: &GpuTexture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let rgba = readback_srgb_rgba8(device, texture, width, height);
    encode_rgba8_png(&rgba, width, height)
}
