// Compute variant of reaction_diffusion_sim.wgsl — same Gray-Scott math,
// reads source via textureSampleLevel, writes target via textureStore.

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
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

// Hash for seeding — exact port of Unity's hash21
fn hash21(p_in: vec2<f32>) -> f32 {
    var p = fract(p_in * vec2<f32>(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Scale kernel: uv_scale = 1/Scale, dividing makes kernel bigger at higher Scale
    let uvs = max(u.uv_scale, 0.01);
    let texel = vec2<f32>(u.texel_x, u.texel_y) / uvs;

    let tx = vec2<f32>(texel.x, 0.0);
    let ty = vec2<f32>(0.0, texel.y);

    let c  = textureSampleLevel(state_tex, state_sampler, uv, 0.0);
    var a = c.r;
    var bb = c.g;

    // Seed B in scattered spots during first few frames (time < 0.1)
    if u.time_val < 0.1 {
        let seed_grid = 20.0 * uvs;
        let cell = floor(uv * seed_grid);
        let h = hash21(cell);
        let cell_center = (cell + 0.5) / seed_grid;
        let dist = length(uv - cell_center) * seed_grid;
        if h > 0.85 && dist < 0.3 {
            bb = 1.0;
            a = 0.5;
        }
    }

    // 3x3 weighted Laplacian: 0.2 cardinal, 0.05 diagonal, -1.0 center
    // Cardinal neighbors (weight 0.2 each)
    let l  = textureSampleLevel(state_tex, state_sampler, uv - tx, 0.0);
    let r  = textureSampleLevel(state_tex, state_sampler, uv + tx, 0.0);
    let t  = textureSampleLevel(state_tex, state_sampler, uv - ty, 0.0);
    let b  = textureSampleLevel(state_tex, state_sampler, uv + ty, 0.0);

    var lap_a = l.r * 0.2 + r.r * 0.2 + t.r * 0.2 + b.r * 0.2;
    var lap_b = l.g * 0.2 + r.g * 0.2 + t.g * 0.2 + b.g * 0.2;

    // Diagonal neighbors (weight 0.05 each)
    let tl = textureSampleLevel(state_tex, state_sampler, uv - tx - ty, 0.0);
    let tr = textureSampleLevel(state_tex, state_sampler, uv + tx - ty, 0.0);
    let bl = textureSampleLevel(state_tex, state_sampler, uv - tx + ty, 0.0);
    let br = textureSampleLevel(state_tex, state_sampler, uv + tx + ty, 0.0);

    lap_a += tl.r * 0.05 + tr.r * 0.05 + bl.r * 0.05 + br.r * 0.05;
    lap_b += tl.g * 0.05 + tr.g * 0.05 + bl.g * 0.05 + br.g * 0.05;

    // Subtract center (total neighbor weight = 4*0.2 + 4*0.05 = 1.0)
    lap_a -= c.r;
    lap_b -= c.g;

    // Gray-Scott update (Pearson's classic diffusion rates)
    let da = 0.2097;
    let db = 0.105;
    let dt = clamp(u.anim_speed, 0.1, 1.5);
    let abb = a * bb * bb;

    var new_a = a + (da * lap_a - abb + u.feed * (1.0 - a)) * dt;
    var new_b = bb + (db * lap_b + abb - (u.kill + u.feed) * bb) * dt;

    new_a = clamp(new_a, 0.0, 1.0);
    new_b = clamp(new_b, 0.0, 1.0);

    textureStore(output_tex, gid.xy, vec4<f32>(new_a, new_b, 0.0, 1.0));
}
