// Simplified physarum approximation via stateful ping-pong fragment shader.
// Uses reaction-diffusion-like feedback with directional probes to approximate
// the branching patterns of full agent-based Physarum simulation.

struct Uniforms {
    time_val: f32,
    sens_dist: f32,
    sens_angle: f32,
    turn: f32,
    step_size: f32,
    deposit: f32,
    decay: f32,
    color_hue: f32,
    glow: f32,
    reactivity: f32,
    scale: f32,
    seeds: f32,
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

const PI: f32 = 3.14159265;

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract((p3.x + p3.y) * p3.z);
}

fn hash22(p: vec2<f32>) -> vec2<f32> {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * vec3<f32>(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract(vec2<f32>((p3.x + p3.y) * p3.z, (p3.x + p3.z) * p3.y));
}

fn noise2d(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let uu = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, uu.x), mix(c, d, uu.x), uu.y);
}

// HSV to RGB conversion
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
    let t = u.time_val;
    let texel = vec2<f32>(u.texel_x, u.texel_y);

    // Read previous state: R = trail concentration, G = heading angle
    let prev = textureSample(state_tex, state_sampler, uv);
    var trail = prev.r;
    var heading = prev.g * PI * 2.0; // Stored as [0,1] mapped to [0, 2*PI]

    // Generate a pseudo-random heading for this pixel based on position + time noise
    let noise_heading = noise2d(uv * 50.0 * u.scale + vec2<f32>(t * 0.1, 0.0)) * PI * 2.0;
    heading = mix(heading, noise_heading, u.reactivity * 0.3);

    // Three directional probes (left, center, right)
    let probe_dist = u.sens_dist * 10.0; // Scale to texels
    let angle_offset = u.sens_angle;

    let dir_l = vec2<f32>(cos(heading - angle_offset), sin(heading - angle_offset));
    let dir_c = vec2<f32>(cos(heading), sin(heading));
    let dir_r = vec2<f32>(cos(heading + angle_offset), sin(heading + angle_offset));

    let sample_l = textureSample(state_tex, state_sampler, uv + dir_l * texel * probe_dist).r;
    let sample_c = textureSample(state_tex, state_sampler, uv + dir_c * texel * probe_dist).r;
    let sample_r = textureSample(state_tex, state_sampler, uv + dir_r * texel * probe_dist).r;

    // Turn toward highest trail concentration
    if sample_l > sample_c && sample_l > sample_r {
        heading -= u.turn;
    } else if sample_r > sample_c && sample_r > sample_l {
        heading += u.turn;
    }

    // Advect: sample state from opposite movement direction (backward advection)
    let move_dir = vec2<f32>(cos(heading), sin(heading));
    let advect_uv = uv - move_dir * texel * u.step_size * 500.0;
    let advected = textureSample(state_tex, state_sampler, advect_uv).r;

    // Diffuse: 3x3 blur with decay
    let tx = vec2<f32>(u.texel_x, 0.0);
    let ty = vec2<f32>(0.0, u.texel_y);
    let blur = (
        textureSample(state_tex, state_sampler, uv - tx).r +
        textureSample(state_tex, state_sampler, uv + tx).r +
        textureSample(state_tex, state_sampler, uv - ty).r +
        textureSample(state_tex, state_sampler, uv + ty).r
    ) * 0.2 + trail * 0.2;

    // Deposit trail
    let deposit_amount = u.deposit * 0.01;
    trail = mix(advected, blur, 0.5) * u.decay + deposit_amount;

    // Seeding: place initial trail spots
    if t < 0.15 {
        let grid = floor(uv * u.seeds * 4.0);
        let h = hash21(grid + vec2<f32>(42.0, 17.0));
        if h > 0.7 {
            let local = fract(uv * u.seeds * 4.0) - vec2<f32>(0.5);
            if dot(local, local) < 0.06 {
                trail = 1.0;
            }
        }
    }

    trail = clamp(trail, 0.0, 1.0);
    let stored_heading = fract(heading / (PI * 2.0));

    // Store trail in R, heading in G, luminance display in B
    // Display: HSV coloring with glow
    let display_lum = pow(trail, 1.0 / max(u.glow, 0.1));
    let col = hsv2rgb(u.color_hue, 0.7 * trail, display_lum);

    return vec4<f32>(trail, stored_heading, col.r * 0.3 + col.g * 0.59 + col.b * 0.11, 1.0);
}
