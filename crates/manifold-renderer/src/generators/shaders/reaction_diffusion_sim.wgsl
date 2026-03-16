struct Uniforms {
    time_val: f32,
    feed: f32,
    kill: f32,
    anim_speed: f32,
    uv_scale: f32,
    texel_x: f32,
    texel_y: f32,
    _pad: f32,
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

// Simple hash for seeding
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 += dot(p3, vec3<f32>(p3.y + 33.33, p3.z + 33.33, p3.x + 33.33));
    return fract((p3.x + p3.y) * p3.z);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let tx = vec2<f32>(u.texel_x, 0.0);
    let ty = vec2<f32>(0.0, u.texel_y);

    let c  = textureSample(state_tex, state_sampler, uv);
    let l  = textureSample(state_tex, state_sampler, uv - tx);
    let r  = textureSample(state_tex, state_sampler, uv + tx);
    let t  = textureSample(state_tex, state_sampler, uv - ty);
    let b  = textureSample(state_tex, state_sampler, uv + ty);
    let tl = textureSample(state_tex, state_sampler, uv - tx - ty);
    let tr = textureSample(state_tex, state_sampler, uv + tx - ty);
    let bl = textureSample(state_tex, state_sampler, uv - tx + ty);
    let br = textureSample(state_tex, state_sampler, uv + tx + ty);

    // 3x3 weighted Laplacian: 0.2 cardinal, 0.05 diagonal, -1.0 center
    let lap_a = (l.r + r.r + t.r + b.r) * 0.2
              + (tl.r + tr.r + bl.r + br.r) * 0.05
              - c.r;
    let lap_b = (l.g + r.g + t.g + b.g) * 0.2
              + (tl.g + tr.g + bl.g + br.g) * 0.05
              - c.g;

    let a = c.r;
    let bb = c.g;
    let da = 0.2097;
    let db = 0.105;
    let dt = clamp(u.anim_speed, 0.1, 1.5);

    let feed = u.feed;
    let kill = u.kill;

    // Gray-Scott reaction-diffusion
    let abb = a * bb * bb;
    var new_a = a + (da * lap_a - abb + feed * (1.0 - a)) * dt;
    var new_b = bb + (db * lap_b + abb - (kill + feed) * bb) * dt;

    // Seeding: inject B in scattered spots when time < 0.1
    if u.time_val < 0.1 {
        let grid = floor(uv * 16.0);
        let h = hash21(grid);
        if h > 0.85 {
            let local = fract(uv * 16.0) - vec2<f32>(0.5);
            if dot(local, local) < 0.04 {
                new_a = 0.5;
                new_b = 0.25;
            }
        }
    }

    new_a = clamp(new_a, 0.0, 1.0);
    new_b = clamp(new_b, 0.0, 1.0);

    return vec4<f32>(new_a, new_b, 0.0, 1.0);
}
