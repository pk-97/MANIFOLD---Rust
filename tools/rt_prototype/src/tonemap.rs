//! Tonemap + sRGB encode + PNG write, BRIEF.md step 7. Amendment (review):
//! matches the app's actual curve, not an invented approximation — see
//! `aces_narkowicz_raw`/`tonemap_sdr`'s default (curve 0) branch in
//! `crates/manifold-renderer/src/effects/shaders/aces_tonemap_compute.wgsl`
//! (mode 0 = SDR, curve 0 = Narkowicz, `TonemapSettings::default()` in
//! `crates/manifold-renderer/src/tonemap.rs`). Same constants, same
//! `saturate()` clamp; the sRGB OETF below is the standard IEC 61966-2-1
//! curve, applied here because the app's WGSL never encodes sRGB itself —
//! that happens implicitly when the compositor writes to an `_sRGB`-tagged
//! swapchain texture, which this offline PNG writer has no equivalent of.

fn aces_narkowicz_raw(x: f32) -> f32 {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
}

fn linear_to_srgb(x: f32) -> f32 {
    if x <= 0.0031308 {
        (x * 12.92).clamp(0.0, 1.0)
    } else {
        (1.055 * x.powf(1.0 / 2.4) - 0.055).clamp(0.0, 1.0)
    }
}

/// Tonemap+encode an rgba float buffer (linear HDR) and write as PNG.
/// Returns the mean pixel value across all channels [0,255] for the
/// smoke-test's non-black check.
pub fn write_png(path: &std::path::Path, pixels: &[[f32; 4]], width: u32, height: u32) -> f64 {
    let mut bytes = vec![0u8; (width * height * 3) as usize];
    let mut sum = 0f64;
    for (i, px) in pixels.iter().enumerate() {
        for c in 0..3 {
            let v = linear_to_srgb(aces_narkowicz_raw(px[c]));
            let b = (v * 255.0).round().clamp(0.0, 255.0) as u8;
            bytes[i * 3 + c] = b;
            sum += b as f64;
        }
    }
    let file = std::fs::File::create(path).unwrap_or_else(|e| panic!("create {path:?}: {e}"));
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("png header");
    writer.write_image_data(&bytes).expect("png write");
    sum / (width * height * 3) as f64
}
