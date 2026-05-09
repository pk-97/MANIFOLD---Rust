// Sample-and-slice compute shader for tv-led-mirror.
//
// Reads the captured-screen IOSurface as Bgra8Unorm (sRGB-encoded bytes
// interpreted as linear values), maps each output column to a slice of the
// LEFT or RIGHT edge of the source per `left_edge_width` / `right_edge_width`,
// blurs (5×5 binomial), and decodes sRGB→linear before writing to the
// strip×LED output texture. The downstream LED edge-extend pass treats this
// strip×LED texture as a per-strip pre-sliced source (its hardcoded 0.5/0.5
// widths act as identity at strip-aligned input resolution), so DMX bytes end
// up matching the screen's PERCEPTUAL brightness instead of its sRGB byte
// values — i.e. mid-grey on the TV becomes mid-grey on the LEDs, not white.

struct Uniforms {
    left_edge_width: f32,
    right_edge_width: f32,
    blur_radius: f32,    // in source texels; 0 = no blur
    decode_srgb: f32,    // 1.0 = decode sRGB→linear, 0.0 = passthrough
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba8unorm, write>;

fn srgb_to_linear_channel(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let raw_uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Half the strips sample the left edge, half the right.
    var source_u: f32;
    if raw_uv.x < 0.5 {
        source_u = (raw_uv.x / 0.5) * uniforms.left_edge_width;
    } else {
        source_u = (1.0 - uniforms.right_edge_width)
            + ((raw_uv.x - 0.5) / 0.5) * uniforms.right_edge_width;
    }
    let center = vec2<f32>(source_u, raw_uv.y);

    var color: vec4<f32>;
    if uniforms.blur_radius <= 0.0 {
        color = textureSampleLevel(source_tex, tex_sampler, center, 0.0);
    } else {
        // Separable-ish 5-tap binomial sampled in 2D (5×5 = 25 taps).
        // Inter-tap spacing scales with `blur_radius` in source-texel units.
        let tex_size = vec2<f32>(textureDimensions(source_tex, 0));
        let r = uniforms.blur_radius / tex_size;
        let w = array<f32, 5>(1.0, 4.0, 6.0, 4.0, 1.0);
        var sum = vec4<f32>(0.0);
        var total_w = 0.0;
        for (var dy = 0; dy < 5; dy = dy + 1) {
            for (var dx = 0; dx < 5; dx = dx + 1) {
                let ox = (f32(dx) - 2.0) * r.x;
                let oy = (f32(dy) - 2.0) * r.y;
                let weight = w[dx] * w[dy];
                sum = sum + textureSampleLevel(
                    source_tex, tex_sampler,
                    center + vec2<f32>(ox, oy),
                    0.0,
                ) * weight;
                total_w = total_w + weight;
            }
        }
        color = sum / total_w;
    }

    if uniforms.decode_srgb >= 0.5 {
        color = vec4<f32>(
            srgb_to_linear_channel(color.r),
            srgb_to_linear_channel(color.g),
            srgb_to_linear_channel(color.b),
            color.a,
        );
    }

    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
