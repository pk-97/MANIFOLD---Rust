// Compute variant of fx_film_grain.wgsl — same math, no TBDR tile overhead.
// Hash-based temporal noise with luma-weighted intensity.
// Unity ref: FilmGrainEffect.shader

struct Uniforms {
    amount: f32,
    grain_size: f32,
    luma_weight: f32,
    color_grain: f32,
    time: f32,
    resolution: vec2<f32>,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

// Hash-based pseudo-random [0,1] — matches Unity FilmGrainEffect.shader hash()
fn hash(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

// Per-channel seeded hash — matches Unity FilmGrainEffect.shader hash3seed()
fn hash3seed(p: vec2<f32>, seed: f32) -> f32 {
    return hash(p + vec2<f32>(seed * 127.1, seed * 311.7));
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    // Grain coordinates: UV scaled by resolution / grain size, with temporal seed
    let grain_uv = uv * uniforms.resolution / uniforms.grain_size;
    let time_seed = floor(uniforms.time * 24.0); // 24 fps grain update

    // Generate mono noise [-1,1]
    let n_mono = hash(grain_uv + time_seed) * 2.0 - 1.0;

    // Generate per-channel color noise [-1,1] (Unity: FilmGrainEffect.shader lines 82-84)
    let n_r = hash3seed(grain_uv, time_seed) * 2.0 - 1.0;
    let n_g = hash3seed(grain_uv, time_seed + 1.0) * 2.0 - 1.0;
    let n_b = hash3seed(grain_uv, time_seed + 2.0) * 2.0 - 1.0;

    // Blend mono vs color grain (Unity: FilmGrainEffect.shader line 87)
    let grain = mix(vec3<f32>(n_mono, n_mono, n_mono), vec3<f32>(n_r, n_g, n_b), uniforms.color_grain);

    // Luma-weighted intensity: stronger in shadows (filmic behavior)
    let luma = dot(src.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let luma_factor = mix(1.0, 1.0 - luma, uniforms.luma_weight);

    // Overlay blend: preserves midtones, adds texture
    let grain_strength = uniforms.amount * 0.3 * luma_factor;
    let effected = src.rgb + grain * grain_strength;

    let result = mix(src.rgb, effected, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
