// LED edge-extend shader with spatial blur — compute dispatch variant.
// Identical math to led_edge_extend.wgsl. Only the input/output mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment
// Unity equivalent: LEDEdgeExtend.shader (enhanced with blur)

struct Uniforms {
    left_edge_width: f32,
    right_edge_width: f32,
    // Blur radius in source texels. 0 = no blur (single sample).
    blur_radius: f32,
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let raw_uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let uv = vec2<f32>(raw_uv.x, 1.0 - raw_uv.y);

    // Compute source U coordinate from edge-extend mapping
    var source_u: f32;
    if uv.x < 0.5 {
        source_u = (uv.x / 0.5) * uniforms.left_edge_width;
    } else {
        source_u = (1.0 - uniforms.right_edge_width)
            + ((uv.x - 0.5) / 0.5) * uniforms.right_edge_width;
    }

    let center = vec2<f32>(source_u, uv.y);

    // No blur: single sample (fast path)
    if uniforms.blur_radius <= 0.0 {
        let color = textureSampleLevel(source_tex, tex_sampler, center, 0.0);
        textureStore(output_tex, vec2<i32>(gid.xy), color);
        return;
    }

    // Vertical-only blur. The LED source is rendered at native strip×LED
    // resolution (one column per physical strip), so blurring horizontally
    // would bleed colour between adjacent strips. Blurring vertically (along
    // the strip) smooths the distribution between LEDs without disturbing
    // the per-strip mapping. 5-tap binomial weights (1,4,6,4,1)/16 give a
    // Gaussian-ish kernel; `blur_radius` scales the inter-tap spacing in
    // source-texel units.
    let tex_size = vec2<f32>(textureDimensions(source_tex, 0));
    let texel_y = 1.0 / tex_size.y;
    let r = uniforms.blur_radius;

    let s_n2 = textureSampleLevel(source_tex, tex_sampler, center + vec2<f32>(0.0, -2.0 * r * texel_y), 0.0);
    let s_n1 = textureSampleLevel(source_tex, tex_sampler, center + vec2<f32>(0.0, -1.0 * r * texel_y), 0.0);
    let s_0  = textureSampleLevel(source_tex, tex_sampler, center, 0.0);
    let s_p1 = textureSampleLevel(source_tex, tex_sampler, center + vec2<f32>(0.0,  1.0 * r * texel_y), 0.0);
    let s_p2 = textureSampleLevel(source_tex, tex_sampler, center + vec2<f32>(0.0,  2.0 * r * texel_y), 0.0);

    let color = (s_n2 * 1.0 + s_n1 * 4.0 + s_0 * 6.0 + s_p1 * 4.0 + s_p2 * 1.0) / 16.0;
    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
