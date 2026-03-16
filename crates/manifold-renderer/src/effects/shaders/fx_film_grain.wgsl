// FilmGrain effect — hash-based temporal noise with luma-weighted intensity.

struct Uniforms {
    amount: f32,
    grain_size: f32,
    luma_weight: f32,
    time: f32,
    resolution: vec2<f32>,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

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

fn hash(p: vec2<f32>) -> f32 {
    let p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    let d = dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract((p3.x + p3.y) * (p3.z + d));
}

fn hash3seed(p: vec2<f32>, seed: f32) -> f32 {
    return hash(p + vec2<f32>(seed * 127.1, seed * 311.7));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);

    // Grain coordinates: UV scaled by resolution / grain size, with temporal seed
    let grain_uv = in.uv * uniforms.resolution / uniforms.grain_size;
    let time_seed = floor(uniforms.time * 24.0);

    // Generate noise [-1,1]
    let n_mono = hash(grain_uv + time_seed) * 2.0 - 1.0;

    // Monochrome grain (no color grain param — keep it simple)
    let grain = vec3<f32>(n_mono, n_mono, n_mono);

    // Luma-weighted intensity: stronger in shadows (filmic behavior)
    let luma = dot(src.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let luma_factor = mix(1.0, 1.0 - luma, uniforms.luma_weight);

    // Overlay blend: preserves midtones, adds texture
    let grain_strength = uniforms.amount * 0.3 * luma_factor;
    let effected = src.rgb + grain * grain_strength;

    let result = mix(src.rgb, effected, uniforms.amount);
    return vec4<f32>(result, src.a);
}
