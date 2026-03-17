// Mechanical port of Unity BloomEffect.shader.
// Two textures: main_tex (_MainTex) and bloom_tex (_BloomTex).
// Mode 0: fragPrefilter — Blur9(main_tex) → BrightPrefilter
// Mode 1: fragDownsample — Blur9(main_tex)
// Mode 2: fragUpsample — hi=main_tex + lo=Blur13(bloom_tex) * combine_weight
// Mode 3: fragComposite — src=main_tex + Blur13(bloom_tex) * intensity

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
@group(0) @binding(1) var main_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var bloom_tex: texture_2d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

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

// Port of Unity Blur9 (BloomEffect.shader lines 67-80) — reads main_tex
fn blur9(uv: vec2<f32>, texel: vec2<f32>, radius: f32) -> vec3<f32> {
    let r = texel * radius * uniforms.radius_scale;
    let s0 = textureSample(main_tex, tex_sampler, uv).rgb;
    let s1 = textureSample(main_tex, tex_sampler, uv + vec2<f32>( r.x, 0.0)).rgb;
    let s2 = textureSample(main_tex, tex_sampler, uv + vec2<f32>(-r.x, 0.0)).rgb;
    let s3 = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0,  r.y)).rgb;
    let s4 = textureSample(main_tex, tex_sampler, uv + vec2<f32>(0.0, -r.y)).rgb;
    let s5 = textureSample(main_tex, tex_sampler, uv + vec2<f32>( r.x,  r.y)).rgb;
    let s6 = textureSample(main_tex, tex_sampler, uv + vec2<f32>(-r.x,  r.y)).rgb;
    let s7 = textureSample(main_tex, tex_sampler, uv + vec2<f32>( r.x, -r.y)).rgb;
    let s8 = textureSample(main_tex, tex_sampler, uv + vec2<f32>(-r.x, -r.y)).rgb;
    return (s0 * 0.24) + ((s1 + s2 + s3 + s4) * 0.12) + ((s5 + s6 + s7 + s8) * 0.07);
}

// Port of Unity Blur13 (BloomEffect.shader lines 83-104) — reads bloom_tex
fn blur13(uv: vec2<f32>, texel: vec2<f32>, radius: f32) -> vec3<f32> {
    let r = texel * radius * uniforms.radius_scale;
    var acc = textureSample(bloom_tex, tex_sampler, uv).rgb * 0.16;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>( r.x, 0.0)).rgb * 0.11;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(-r.x, 0.0)).rgb * 0.11;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(0.0,  r.y)).rgb * 0.11;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(0.0, -r.y)).rgb * 0.11;

    let r2 = r * 2.0;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>( r2.x, 0.0)).rgb * 0.06;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(-r2.x, 0.0)).rgb * 0.06;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(0.0,  r2.y)).rgb * 0.06;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(0.0, -r2.y)).rgb * 0.06;

    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>( r.x,  r.y)).rgb * 0.04;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(-r.x,  r.y)).rgb * 0.04;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>( r.x, -r.y)).rgb * 0.04;
    acc += textureSample(bloom_tex, tex_sampler, uv + vec2<f32>(-r.x, -r.y)).rgb * 0.04;
    return acc;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let main_ts = vec2<f32>(uniforms.main_texel_size_x, uniforms.main_texel_size_y);
    let bloom_ts = vec2<f32>(uniforms.bloom_texel_size_x, uniforms.bloom_texel_size_y);

    if uniforms.mode == 0u {
        // fragPrefilter (lines 106-112): Blur9(_MainTex) → BrightPrefilter
        let blurred = blur9(in.uv, main_ts, 0.9);
        let bright = bright_prefilter(blurred);
        return vec4<f32>(bright, 1.0);

    } else if uniforms.mode == 1u {
        // fragDownsample (lines 114-119): Blur9(_MainTex)
        let blurred = blur9(in.uv, main_ts, 1.1);
        return vec4<f32>(blurred, 1.0);

    } else if uniforms.mode == 2u {
        // fragUpsample (lines 121-128): hi=_MainTex + lo=Blur13(_BloomTex) * _CombineWeight
        let hi = textureSample(main_tex, tex_sampler, in.uv).rgb;
        let lo = blur13(in.uv, bloom_ts, 0.9);
        let out_val = hi + lo * uniforms.combine_weight;
        return vec4<f32>(out_val, 1.0);

    } else {
        // fragComposite (lines 130-140): src=_MainTex + Blur13(_BloomTex) * _Intensity
        let src_sample = textureSample(main_tex, tex_sampler, in.uv);
        let src_lin = src_sample.rgb;
        let bloom_lin = blur13(in.uv, bloom_ts, 0.7) * uniforms.intensity;
        let out_val = src_lin + bloom_lin;
        return vec4<f32>(out_val, src_sample.a);
    }
}
