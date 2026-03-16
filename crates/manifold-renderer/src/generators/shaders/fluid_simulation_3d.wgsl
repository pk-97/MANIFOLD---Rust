// Simplified 3D fluid simulation: same advection as FluidSimulation but with
// 3D camera projection params applied to UV transformation.

struct Uniforms {
    time_val: f32,
    flow: f32,
    feather: f32,
    curl_angle: f32,
    turbulence: f32,
    speed: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
    texel_x: f32,
    texel_y: f32,
    color_mode: f32,
    color_bright: f32,
    decay: f32,
    cam_dist: f32,
    cam_tilt: f32,
    flatten: f32,
    container: f32,
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

const PI: f32 = 3.14159265;

fn hash22(p: vec2<f32>) -> vec2<f32> {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract(vec2<f32>((p3.x + p3.y) * p3.z, (p3.x + p3.z) * p3.y));
}

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract((p3.x + p3.y) * p3.z);
}

fn noise2d(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let uu = f * f * (3.0 - 2.0 * f);
    let a = dot(hash22(i) * 2.0 - 1.0, f);
    let b = dot(hash22(i + vec2<f32>(1.0, 0.0)) * 2.0 - 1.0, f - vec2<f32>(1.0, 0.0));
    let c = dot(hash22(i + vec2<f32>(0.0, 1.0)) * 2.0 - 1.0, f - vec2<f32>(0.0, 1.0));
    let d = dot(hash22(i + vec2<f32>(1.0, 1.0)) * 2.0 - 1.0, f - vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, uu.x), mix(c, d, uu.x), uu.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var val = 0.0;
    var amp = 0.5;
    var pp = p;
    for (var i = 0; i < 5; i++) {
        val += amp * noise2d(pp);
        pp *= 2.0;
        amp *= 0.5;
    }
    return val;
}

fn curl_noise(p: vec2<f32>, t: f32) -> vec2<f32> {
    let eps = 0.01;
    let pt = p + vec2<f32>(t * 0.05, t * 0.03);
    let n0 = fbm(pt);
    let nx = fbm(pt + vec2<f32>(eps, 0.0));
    let ny = fbm(pt + vec2<f32>(0.0, eps));
    return vec2<f32>((ny - n0) / eps, -(nx - n0) / eps);
}

fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let hp = h * 6.0;
    let x = c * (1.0 - abs(hp % 2.0 - 1.0));
    var rgb = vec3<f32>(0.0);
    if hp < 1.0 { rgb = vec3<f32>(c, x, 0.0); }
    else if hp < 2.0 { rgb = vec3<f32>(x, c, 0.0); }
    else if hp < 3.0 { rgb = vec3<f32>(0.0, c, x); }
    else if hp < 4.0 { rgb = vec3<f32>(0.0, x, c); }
    else if hp < 5.0 { rgb = vec3<f32>(x, 0.0, c); }
    else { rgb = vec3<f32>(c, 0.0, x); }
    return rgb + vec3<f32>(v - c);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let t = u.time_val * u.speed;
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    // Apply 3D camera perspective distortion to UV
    var p = uv * 2.0 - 1.0;

    // Camera tilt: compress Y based on tilt angle
    let tilt_factor = 1.0 - abs(u.cam_tilt) * 0.4;
    p.y = p.y * tilt_factor + u.cam_tilt * 0.2;

    // Camera distance: zoom effect
    p = p / u.cam_dist;

    // Flatten: compress depth dimension (manifests as reduced Y range)
    p.y = p.y * (1.0 - u.flatten * 0.5);

    // Container: vignette / boundary mask
    let container_mask = 1.0 - smoothstep(0.6 + u.container * 0.15, 0.95 + u.container * 0.05, length(p));

    // Map back to UV space for state texture sampling
    let sim_uv = p * 0.5 + 0.5;

    // Scale for noise sampling
    var np = p * u.uv_scale;

    // Curl noise advection
    let curl_angle_rad = u.curl_angle * PI / 180.0;
    var curl = curl_noise(np * u.feather * 0.1, t);
    let ca = cos(curl_angle_rad);
    let sa = sin(curl_angle_rad);
    curl = vec2<f32>(curl.x * ca - curl.y * sa, curl.x * sa + curl.y * ca);

    // Density gradient flow
    let tx = vec2<f32>(u.texel_x, 0.0);
    let ty = vec2<f32>(0.0, u.texel_y);
    let d_left = textureSample(state_tex, state_sampler, sim_uv - tx).r;
    let d_right = textureSample(state_tex, state_sampler, sim_uv + tx).r;
    let d_up = textureSample(state_tex, state_sampler, sim_uv - ty).r;
    let d_down = textureSample(state_tex, state_sampler, sim_uv + ty).r;
    let grad = vec2<f32>(d_right - d_left, d_down - d_up);

    let flow_vec = grad * u.flow * 100.0;

    // Turbulence
    let turb = vec2<f32>(
        noise2d(np * 8.0 + vec2<f32>(t * 0.7, 0.0)),
        noise2d(np * 8.0 + vec2<f32>(0.0, t * 0.7))
    ) * u.turbulence * 10.0;

    let advect = curl * 0.5 + flow_vec + turb;
    let advect_uv = sim_uv - advect * texel * 30.0;
    let prev = textureSample(state_tex, state_sampler, advect_uv);

    var density = prev.r * 0.97;

    // Injection
    let inject_noise = fbm(np * u.feather * 0.2 + vec2<f32>(t * 0.2, t * 0.15));
    let injection = smoothstep(0.2, 0.5, inject_noise) * 0.03;
    density = clamp(density + injection, 0.0, 1.0);

    // Apply container boundary
    density *= container_mask;

    // Seeding
    if t < 0.2 {
        let grid = floor(sim_uv * 8.0);
        let h = hash21(grid + vec2<f32>(7.0, 13.0));
        if h > 0.75 {
            let local = fract(sim_uv * 8.0) - vec2<f32>(0.5);
            if dot(local, local) < 0.08 {
                density = container_mask;
            }
        }
    }

    // Tone mapping
    let x_val = density * u.contrast;
    var lum = x_val * (1.0 + x_val / 9.0) / (1.0 + x_val);

    if u.invert > 0.5 {
        lum = 1.0 - lum;
    }

    // Color
    var col = vec3<f32>(lum);
    if u.color_mode > 0.5 {
        let hue = fract(u.color_mode * 0.2 + density * 0.3);
        col = hsv2rgb(hue, 0.6 * density, lum * u.color_bright);
    }

    return vec4<f32>(density, col.r * 0.3 + col.g * 0.59 + col.b * 0.11, lum, 1.0);
}
