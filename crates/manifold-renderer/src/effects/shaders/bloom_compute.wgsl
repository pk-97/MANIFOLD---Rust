// Compute variant of bloom.wgsl for ComputeDualBlitHelper.
// Two source textures: source_tex_a (_MainTex) and source_tex_b (_BloomTex).
// Mode 0: Blur9(source_a) -> BrightPrefilter
// Mode 1: Blur9(source_a)
// Mode 2: hi=source_a + lo=Blur13(source_b) * combine_weight
// Mode 3: src=source_a + Blur13(source_b) * intensity

struct Uniforms {
    mode: u32,
    threshold: f32,
    knee: f32,
    intensity: f32,
    radius_scale: f32,
    combine_weight: f32,
    main_texel_size_x: f32,
    main_texel_size_y: f32,
    bloom_texel_size_x: f32,
    bloom_texel_size_y: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex_a: texture_2d<f32>;
@group(0) @binding(2) var source_tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

// Port of Unity BrightPrefilter (BloomEffect.shader lines 55-64)
fn bright_prefilter(col_lin: vec3<f32>) -> vec3<f32> {
    let lum = max(col_lin.r, max(col_lin.g, col_lin.b));
    let soft_start = uniforms.threshold - uniforms.knee;
    var t = clamp((lum - soft_start) / max(2.0 * uniforms.knee, 1e-5), 0.0, 1.0);
    t = t * t * (3.0 - 2.0 * t);
    let hard = clamp((lum - uniforms.threshold) / max(1.0 - uniforms.threshold, 1e-5), 0.0, 1.0);
    let response = max(t * 0.78, hard);
    return col_lin * response;
}

// Port of Unity Blur9 (BloomEffect.shader lines 67-80) — reads source_tex_a
fn blur9(uv: vec2<f32>, texel: vec2<f32>, radius: f32) -> vec3<f32> {
    let r = texel * radius * uniforms.radius_scale;
    let s0 = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0).rgb;
    let s1 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r.x, 0.0), 0.0).rgb;
    let s2 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r.x, 0.0), 0.0).rgb;
    let s3 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0,  r.y), 0.0).rgb;
    let s4 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(0.0, -r.y), 0.0).rgb;
    let s5 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r.x,  r.y), 0.0).rgb;
    let s6 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r.x,  r.y), 0.0).rgb;
    let s7 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>( r.x, -r.y), 0.0).rgb;
    let s8 = textureSampleLevel(source_tex_a, tex_sampler, uv + vec2<f32>(-r.x, -r.y), 0.0).rgb;
    return (s0 * 0.24) + ((s1 + s2 + s3 + s4) * 0.12) + ((s5 + s6 + s7 + s8) * 0.07);
}

// Port of Unity Blur13 (BloomEffect.shader lines 83-104) — reads source_tex_b
fn blur13(uv: vec2<f32>, texel: vec2<f32>, radius: f32) -> vec3<f32> {
    let r = texel * radius * uniforms.radius_scale;
    var acc = textureSampleLevel(source_tex_b, tex_sampler, uv, 0.0).rgb * 0.16;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>( r.x, 0.0), 0.0).rgb * 0.11;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(-r.x, 0.0), 0.0).rgb * 0.11;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(0.0,  r.y), 0.0).rgb * 0.11;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(0.0, -r.y), 0.0).rgb * 0.11;

    let r2 = r * 2.0;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>( r2.x, 0.0), 0.0).rgb * 0.06;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(-r2.x, 0.0), 0.0).rgb * 0.06;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(0.0,  r2.y), 0.0).rgb * 0.06;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(0.0, -r2.y), 0.0).rgb * 0.06;

    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>( r.x,  r.y), 0.0).rgb * 0.04;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(-r.x,  r.y), 0.0).rgb * 0.04;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>( r.x, -r.y), 0.0).rgb * 0.04;
    acc += textureSampleLevel(source_tex_b, tex_sampler, uv + vec2<f32>(-r.x, -r.y), 0.0).rgb * 0.04;
    return acc;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    let main_ts = vec2<f32>(uniforms.main_texel_size_x, uniforms.main_texel_size_y);
    let bloom_ts = vec2<f32>(uniforms.bloom_texel_size_x, uniforms.bloom_texel_size_y);

    var color: vec4<f32>;

    if uniforms.mode == 0u {
        // fragPrefilter: Blur9(source_a) -> BrightPrefilter
        let blurred = blur9(uv, main_ts, 0.9);
        let bright = bright_prefilter(blurred);
        color = vec4<f32>(bright, 1.0);

    } else if uniforms.mode == 1u {
        // fragDownsample: Blur9(source_a)
        let blurred = blur9(uv, main_ts, 1.1);
        color = vec4<f32>(blurred, 1.0);

    } else if uniforms.mode == 2u {
        // fragUpsample: hi=source_a + lo=Blur13(source_b) * combine_weight
        let hi = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0).rgb;
        let lo = blur13(uv, bloom_ts, 0.9);
        let out_val = hi + lo * uniforms.combine_weight;
        color = vec4<f32>(out_val, 1.0);

    } else {
        // fragComposite: src=source_a + Blur13(source_b) * intensity
        let src_sample = textureSampleLevel(source_tex_a, tex_sampler, uv, 0.0);
        let src_lin = src_sample.rgb;
        let bloom_lin = blur13(uv, bloom_ts, 0.7) * uniforms.intensity;
        let out_val = src_lin + bloom_lin;
        color = vec4<f32>(out_val, src_sample.a);
    }

    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
