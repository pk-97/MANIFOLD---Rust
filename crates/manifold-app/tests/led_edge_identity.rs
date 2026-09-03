//! MVP-P1 edge-identity pinning (LED_STRIPS_DESIGN.md section 5b, D13
//! erratum): the controller already forces edge-extend widths to (0.5, 0.5)
//! for ALL routes (`artnet.rs`), which makes the shader's horizontal mapping
//! an identity (source_u == uv.x). This test pins that behavior against
//! `manifold_led`'s public API so a shader or controller change that
//! re-introduces edge cropping fails loudly here instead of mangling
//! direct-drive content on the rig. `manifold-led` itself stays untouched.

use manifold_gpu::{
    GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_led::blit::EdgeExtendBlit;

const STRIPS: u32 = 8;
const LEDS: u32 = 120;

// f16-exact eighths, skipping 4/8 (= 0.5, an exact unorm rounding tie).
// Indexing varies with both x and y so a horizontal rescale OR a vertical
// flip regression changes the expected bytes.
const PAT: [f32; 6] = [0.125, 0.25, 0.375, 0.625, 0.75, 0.875];

fn pat_value(x: u32, y: u32) -> f32 {
    PAT[((x + y) % 6) as usize]
}

/// The rgba8unorm byte the shader must store for a channel value (linear
/// gain 1.0, no channel above 1.0, so gain_and_clip is a no-op).
fn quant(v: f32) -> u8 {
    (v * 255.0).round() as u8
}

#[test]
fn edge_extend_half_widths_is_identity_mapping() {
    let device = GpuDevice::new();

    // Source at the shape the controller feeds the blit: the HDR LED
    // composite, here Rgba16Float at the native grid.
    let source = device.create_texture(&GpuTextureDesc {
        width: STRIPS,
        height: LEDS,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label: "edge-identity-src",
        mip_levels: 1,
    });
    let mut pixels = Vec::new();
    for y in 0..LEDS {
        for x in 0..STRIPS {
            let v = half::f16::from_f32(pat_value(x, y));
            for channel in [v, v, v, half::f16::from_f32(1.0)] {
                pixels.extend_from_slice(&channel.to_bits().to_le_bytes());
            }
        }
    }
    device.upload_texture(&source, &pixels);

    let blit = EdgeExtendBlit::new(&device, STRIPS, LEDS);
    let mut enc = device.create_encoder("edge-identity-test");
    // The exact controller call: identity widths, no blur, unity gain.
    blit.blit(&mut enc, &source, 0.5, 0.5, 0.0, 1.0);
    enc.commit_and_wait_completed();

    let bytes_per_row = STRIPS * 4;
    let buf = device.create_buffer_shared(u64::from(LEDS * bytes_per_row));
    let mut read_enc = device.create_encoder("edge-identity-readback");
    read_enc.copy_texture_to_buffer(blit.output_texture(), &buf, STRIPS, LEDS, bytes_per_row);
    read_enc.commit_and_wait_completed();
    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let out = unsafe {
        std::slice::from_raw_parts(ptr.cast::<u8>(), (LEDS * bytes_per_row) as usize)
    };

    for y in 0..LEDS {
        for x in 0..STRIPS {
            // At widths (0.5, 0.5) the shader maps source_u == uv.x exactly
            // (no horizontal rescale) and flips vertically (uv.y = 1 - raw),
            // which is the established sampling convention the controller
            // relies on.
            let expected = quant(pat_value(x, LEDS - 1 - y));
            let off = ((y * STRIPS + x) * 4) as usize;
            assert_eq!(
                out[off],
                expected,
                "red channel at ({x},{y}): identity mapping broken"
            );
            assert_eq!(out[off + 1], expected, "green at ({x},{y})");
            assert_eq!(out[off + 2], expected, "blue at ({x},{y})");
            assert_eq!(out[off + 3], 255, "alpha at ({x},{y})");
        }
    }
}
