struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    noise_scale: f32,
    curl_intensity: f32,
    decay: f32,
    texel_x: f32,
    texel_y: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var state_tex: texture_2d<f32>;
@group(0) @binding(2) var state_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// ── Noise functions ──

fn hash22(p: vec2<f32>) -> vec2<f32> {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract(vec2<f32>((p3.x + p3.y) * p3.z, (p3.x + p3.z) * p3.y));
}

fn noise2d(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let uu = f * f * (3.0 - 2.0 * f);

    let a = dot(hash22(i + vec2<f32>(0.0, 0.0)) * 2.0 - 1.0, f - vec2<f32>(0.0, 0.0));
    let b = dot(hash22(i + vec2<f32>(1.0, 0.0)) * 2.0 - 1.0, f - vec2<f32>(1.0, 0.0));
    let c = dot(hash22(i + vec2<f32>(0.0, 1.0)) * 2.0 - 1.0, f - vec2<f32>(0.0, 1.0));
    let d = dot(hash22(i + vec2<f32>(1.0, 1.0)) * 2.0 - 1.0, f - vec2<f32>(1.0, 1.0));

    return mix(mix(a, b, uu.x), mix(c, d, uu.x), uu.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var val = 0.0;
    var amp = 0.5;
    var pp = p;
    for (var i = 0; i < 4; i++) {
        val += amp * noise2d(pp);
        pp *= 2.0;
        amp *= 0.5;
    }
    return val;
}

fn curl_noise(p: vec2<f32>, t: f32) -> vec2<f32> {
    let eps = 0.01;
    let pt = p + vec2<f32>(t * 0.1, t * 0.07);
    let n0 = fbm(pt);
    let nx = fbm(pt + vec2<f32>(eps, 0.0));
    let ny = fbm(pt + vec2<f32>(0.0, eps));
    let dnx = (nx - n0) / eps;
    let dny = (ny - n0) / eps;
    // Curl: rotate gradient 90 degrees
    return vec2<f32>(dny, -dnx);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let t = u.time_val * u.anim_speed;

    // Scale UV for noise sampling
    var p = (uv - vec2<f32>(0.5)) * u.uv_scale;
    p.x *= u.aspect_ratio;

    // Curl noise field
    let curl = curl_noise(p * u.noise_scale, t) * u.curl_intensity;

    // Backwards Euler advection: sample previous state at offset position
    let advect_uv = uv - curl * vec2<f32>(u.texel_x, u.texel_y) * 50.0;
    let prev = textureSample(state_tex, state_sampler, advect_uv);

    // Decay previous state
    let decayed = prev.r * u.decay;

    // Noise-based injection
    let noise_val = fbm(p * u.noise_scale + vec2<f32>(t * 0.3, t * 0.2));
    let injection = smoothstep(0.3, 0.6, noise_val) * (1.0 - u.decay) * 2.0;

    let lum = clamp(decayed + injection, 0.0, 1.0);
    return vec4<f32>(lum, lum, lum, lum);
}
