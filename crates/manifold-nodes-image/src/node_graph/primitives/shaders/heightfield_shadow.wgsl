// node.heightfield_shadow — hand parity oracle for the generated standalone
// kernel (docs/DEPTH_RELIGHT_DESIGN.md D5). Same algorithm as
// heightfield_shadow_body.wgsl — kept independent (not sharing WGSL source)
// so the gpu_tests parity check is a real cross-check, not a tautology.
//
// Bindings match the generated GatherTexel-only layout: uniform(0),
// height_tex(1, textureLoad — no sampler), output_tex(2).

struct Uniforms {
    light_x: f32,
    light_y: f32,
    light_z: f32,
    steps: f32,
    strength: f32,
    softness: f32,
    relief: f32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var height_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

fn hfshadow_round(x: f32) -> f32 {
    if x >= 0.0 {
        return floor(x + 0.5);
    }
    return -floor(-x + 0.5);
}

fn hfshadow_height(c: vec2<i32>, dims_i: vec2<i32>, relief: f32) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(height_tex, cc, 0).r;
    return (1.0 - raw) * relief;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(id.xy);
    let dims_f = vec2<f32>(dims);

    let n_steps = max(1u, u32(hfshadow_round(u.steps)));

    let light_len = length(vec3<f32>(u.light_x, u.light_y, u.light_z));
    var light_dir3 = vec3<f32>(0.0, 0.0, 1.0);
    if light_len > 1e-8 {
        light_dir3 = vec3<f32>(u.light_x, u.light_y, u.light_z) / light_len;
    }
    let xy_len = length(light_dir3.xy);

    if xy_len < 1e-6 {
        textureStore(output_tex, c, vec4<f32>(1.0, 1.0, 1.0, 1.0));
        return;
    }

    let dir2 = light_dir3.xy / xy_len;
    let slope = light_dir3.z / xy_len;

    let start_height = hfshadow_height(c, dims_i, u.relief);

    let max_dist = u.relief * 2.0;
    let max_dist_px = max_dist * dims_f.y;

    var max_penetration = 0.0;
    for (var i: u32 = 1u; i <= n_steps; i = i + 1u) {
        let t = max_dist * f32(i) / f32(n_steps);
        let t_px = max_dist_px * f32(i) / f32(n_steps);
        let offset = vec2<i32>(vec2<f32>(hfshadow_round(dir2.x * t_px), hfshadow_round(-dir2.y * t_px)));
        let cs = clamp(c + offset, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
        let terrain = hfshadow_height(cs, dims_i, u.relief);
        let ray_height = start_height + t * slope;
        let penetration = terrain - ray_height;
        max_penetration = max(max_penetration, penetration);
    }

    if max_penetration <= 0.0 {
        textureStore(output_tex, c, vec4<f32>(1.0, 1.0, 1.0, 1.0));
        return;
    }

    let occlusion = smoothstep(0.0, u.softness * u.relief + 1e-4, max_penetration) * u.strength;
    let lit = clamp(1.0 - occlusion, 0.0, 1.0);
    textureStore(output_tex, c, vec4<f32>(lit, lit, lit, 1.0));
}
