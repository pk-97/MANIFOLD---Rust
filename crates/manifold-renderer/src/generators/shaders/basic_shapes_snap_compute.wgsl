struct Uniforms {
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    trigger_count: f32,
    fill_mode: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

// ── Utility ──

fn rotate2d(p: vec2<f32>, angle: f32) -> vec2<f32> {
    let s = sin(angle);
    let c = cos(angle);
    return vec2<f32>(p.x * c - p.y * s, p.x * s + p.y * c);
}

// ── SDF functions ──

fn sd_square(p: vec2<f32>, size: f32) -> f32 {
    let d = abs(p) - vec2<f32>(size);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0);
}

fn sd_diamond(p: vec2<f32>, size: f32) -> f32 {
    let ap = abs(p);
    return (ap.x + ap.y - size) / 1.414213562;
}

fn sd_octagon(p_in: vec2<f32>, r: f32) -> f32 {
    let k = vec3<f32>(-0.9238795325, 0.3826834323, 0.4142135623);
    var p = abs(p_in);
    p -= 2.0 * min(dot(vec2<f32>(k.x, k.y), p), 0.0) * vec2<f32>(k.x, k.y);
    p -= 2.0 * min(dot(vec2<f32>(-k.x, k.y), p), 0.0) * vec2<f32>(-k.x, k.y);
    p -= vec2<f32>(clamp(p.x, -k.z * r, k.z * r), r);
    return length(p) * sign(p.y);
}

// Evaluate the active SDF at point p for the given shape index.
fn eval_sdf(p: vec2<f32>, shape: u32) -> f32 {
    switch shape {
        case 1u: { return sd_diamond(p, 1.0); }
        case 2u: { return sd_octagon(p, 1.0); }
        default: { return sd_square(p, 1.0); }
    }
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var p_uv = uv - vec2<f32>(0.5);
    p_uv.x *= u.aspect_ratio;
    p_uv *= u.uv_scale;

    let tc = u32(i32(u.trigger_count));

    // Fill mode: 0 = Solid, 1 = Mixed, 2 = Wireframe
    // Mixed cycles 6 variants (3 shapes x 2 fills), matching original behavior
    var shape_idx: u32;
    var is_wireframe: bool;
    var rot_step: u32;

    if u.fill_mode < 0.5 {
        // Solid: cycle 3 shapes, always solid
        shape_idx = tc % 3u;
        is_wireframe = false;
        rot_step = (tc / 3u) % 8u;
    } else if u.fill_mode < 1.5 {
        // Mixed: cycle 6 variants (3 shapes x 2 fills), original snap behavior
        let variant = tc % 6u;
        shape_idx = variant % 3u;
        is_wireframe = variant >= 3u;
        rot_step = (tc / 6u) % 8u;
    } else {
        // Wireframe: cycle 3 shapes, always wireframe
        shape_idx = tc % 3u;
        is_wireframe = true;
        rot_step = (tc / 3u) % 8u;
    }

    // Rotation: 4 angles x 2 directions = 8 steps
    let DEG45 = 0.78539816; // pi/4
    let target_angle = f32(rot_step % 4u) * DEG45;
    let rot_direction = select(1.0, -1.0, rot_step >= 4u);
    let rotation = target_angle * rot_direction;

    // Transform UV
    var p = p_uv / 0.315;
    p = rotate2d(p, rotation);

    // Evaluate SDF at center
    let d = eval_sdf(p, shape_idx);

    // Approximate fwidth(d) via finite differences at one-pixel-right and
    // one-pixel-down neighbors.  The pixel step must follow the full transform
    // chain (aspect → scale → /0.315 → rotate) to land exactly one screen pixel
    // away in SDF space.
    let inv_scale = u.uv_scale / 0.315;
    let step_x = rotate2d(vec2<f32>(inv_scale * u.aspect_ratio / f32(dims.x), 0.0), rotation);
    let step_y = rotate2d(vec2<f32>(0.0, inv_scale / f32(dims.y)), rotation);
    let d_dx = eval_sdf(p + step_x, shape_idx);
    let d_dy = eval_sdf(p + step_y, shape_idx);
    let fw = abs(d_dx - d) + abs(d_dy - d);

    // Smoothstep AA centered at the edge, 1-pixel transition band.
    let half_fw = fw * 0.5;
    var shape: f32;
    if is_wireframe {
        let thickness = u.line_thickness;
        let wd = abs(d) - thickness;
        shape = 1.0 - smoothstep(-half_fw, half_fw, wd);
    } else {
        shape = 1.0 - smoothstep(-half_fw, half_fw, d);
    }

    let lum = saturate(shape);

    textureStore(output, vec2<i32>(id.xy), vec4<f32>(lum, lum, lum, lum));
}
