// primitive.edge_detect — pixel-exact replacement for legacy
// `effects/shaders/fx_edge_detect.wgsl`. Sobel 3×3 edge detection
// with luminance-based gradients and a smoothstep threshold.
// Fused composite — atomic Sobel3 + Threshold would introduce fp16
// rounding at the intermediate write that breaks bit-exact parity
// vs the single-pass legacy. Splits when fusion compiler lands.

struct Uniforms {
    amount: f32,
    threshold: f32,
    texel_size_x: f32,
    texel_size_y: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn sample_lum(uv: vec2<f32>, offset: vec2<f32>) -> f32 {
    let texel = vec2<f32>(uniforms.texel_size_x, uniforms.texel_size_y);
    return luminance(textureSampleLevel(source_tex, tex_sampler, uv + offset * texel, 0.0).rgb);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    let tl = sample_lum(uv, vec2<f32>(-1.0, -1.0));
    let tc = sample_lum(uv, vec2<f32>( 0.0, -1.0));
    let tr = sample_lum(uv, vec2<f32>( 1.0, -1.0));
    let ml = sample_lum(uv, vec2<f32>(-1.0,  0.0));
    let mr = sample_lum(uv, vec2<f32>( 1.0,  0.0));
    let bl = sample_lum(uv, vec2<f32>(-1.0,  1.0));
    let bc = sample_lum(uv, vec2<f32>( 0.0,  1.0));
    let br = sample_lum(uv, vec2<f32>( 1.0,  1.0));

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;

    var edge = sqrt(gx * gx + gy * gy) * 0.25;

    let thresh = uniforms.threshold;
    edge = smoothstep(thresh * 0.5, thresh * 1.5 + 0.01, edge);

    let result = mix(src.rgb, vec3<f32>(edge), uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
