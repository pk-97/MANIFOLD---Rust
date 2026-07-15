//! The ONE shared HDR→PNG output transform for headless render tooling
//! (D2, `docs/GLB_CONFORMANCE_DESIGN.md`). Extracted from
//! `preset_thumbnail.rs`'s established Reinhard-tonemap-and-composite
//! convention — the in-repo precedent `generate_preset_thumbnails.rs`
//! already used — so `render_import`, `generate-preset-thumbnails`, and the
//! glb conformance tests all call exactly one transform instead of each
//! growing its own. **Never reimplement this locally.** The 2026-07-15
//! scratchpad probe had its own Reinhard-without-sRGB-encode readback and
//! rendered systematically darker than the app all session — a harness-local
//! tonemap is precisely the failure mode this module exists to close off
//! (DESIGN_AUTHORING §4's reimplement-and-verify carve-out: share the seam,
//! don't audit the match).

use half::f16;
use manifold_gpu::{GpuDevice, GpuTexture};

/// Read back an `Rgba16Float` target and encode to 8-bit RGBA PNG bytes,
/// composited over opaque black and Reinhard-tonemapped — the SAME
/// convention `preset_thumbnail.rs` established for save-time thumbnails
/// (itself following `mesh_snapshot.rs`'s headless-PNG-dump precedent). THE
/// only tonemap in headless tooling: `render_import`,
/// `generate-preset-thumbnails`, and the conformance tests all call this,
/// never their own.
pub fn readback_to_srgb_png(
    device: &GpuDevice,
    texture: &GpuTexture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let rgba = readback_tonemapped_rgba8(device, texture, width, height);
    encode_rgba8_png(&rgba, width, height)
}

/// Read back an `Rgba16Float` target and Reinhard-tonemap to raw RGBA8 bytes
/// (not yet PNG-encoded) — same convention `mesh_snapshot.rs`'s headless PNG
/// dumps use (this crate's established "linear HDR graph output → a PNG a
/// human can look at" path). `pub` (not `pub(crate)`) because bin targets are
/// separate crates from the lib target — callers that need the raw pixels
/// before encoding (e.g. `render_import`'s convergence check, which measures
/// non-black fraction on the same frame it will eventually write, without a
/// second GPU readback or a PNG decode-and-re-check) reach it through here.
pub fn readback_tonemapped_rgba8(
    device: &GpuDevice,
    tex: &GpuTexture,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/pixel
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("headless-readback");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

    let tonemap = |v: f32| -> u8 {
        let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
        (ldr * 255.0).round() as u8
    };
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in halves.chunks_exact(4) {
        let r = f16::from_bits(px[0]).to_f32();
        let g = f16::from_bits(px[1]).to_f32();
        let b = f16::from_bits(px[2]).to_f32();
        // Composite over OPAQUE BLACK before saving — a transparent PNG
        // reads as white in most viewers (BUG-024). Producers emit straight
        // alpha (alpha-standardisation: producers are not premultiplied), so
        // over black the visible colour is `rgb * a`.
        let a = f16::from_bits(px[3]).to_f32().clamp(0.0, 1.0);
        out.push(tonemap(r * a));
        out.push(tonemap(g * a));
        out.push(tonemap(b * a));
        out.push(255);
    }
    out
}

/// Raw `Rgba16Float` bytes, untouched — no tonemap, no alpha composite. Used
/// only to detect "did the render actually change between frames" for a
/// convergence check (BUG-117): comparing the pre-tonemap bytes catches any
/// change the final PNG would show, and skips the composite math on every
/// candidate frame, not just the one that's written out.
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

/// Non-black fraction of a tonemapped RGBA8 frame (BUG-100/BUG-117
/// convergence style: byte-stability alone can't distinguish "converged" from
/// "every background decode is still mid-load, so three frames in a row are
/// identically black"). Shared so `render_import` and the conformance harness
/// use the same measurement the DamagedHelmet gpu test already proved out.
pub fn non_black_fraction(rgba: &[u8]) -> f64 {
    let pixel_count = rgba.len() / 4;
    if pixel_count == 0 {
        return 0.0;
    }
    let mut non_black = 0usize;
    for px in rgba.chunks_exact(4) {
        if px[0] > 2 || px[1] > 2 || px[2] > 2 {
            non_black += 1;
        }
    }
    non_black as f64 / pixel_count as f64
}

/// PNG-encode already-tonemapped RGBA8 pixels — the pure-CPU second half of
/// [`readback_to_srgb_png`], exposed separately so a caller that already has
/// the pixels (from [`readback_tonemapped_rgba8`]) doesn't pay for a second
/// GPU readback just to get the same bytes PNG-encoded.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_encode_round_trips_a_known_gradient() {
        let w = 4u32;
        let h = 4u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            rgba[i * 4] = (i * 10) as u8;
            rgba[i * 4 + 1] = 20;
            rgba[i * 4 + 2] = 30;
            rgba[i * 4 + 3] = 255;
        }
        let png = encode_rgba8_png(&rgba, w, h);
        let decoded = image::load_from_memory(&png).expect("decode").to_rgba8();
        assert_eq!(decoded.width(), w);
        assert_eq!(decoded.height(), h);
        assert_eq!(decoded.as_raw(), &rgba);
    }

    #[test]
    fn non_black_fraction_all_black_is_zero() {
        let rgba = vec![0u8; 4 * 4 * 4];
        assert_eq!(non_black_fraction(&rgba), 0.0);
    }

    #[test]
    fn non_black_fraction_all_lit_is_one() {
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = 200;
            px[3] = 255;
        }
        assert_eq!(non_black_fraction(&rgba), 1.0);
    }
}
