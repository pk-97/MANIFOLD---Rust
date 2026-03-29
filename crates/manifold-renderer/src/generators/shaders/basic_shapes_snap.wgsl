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

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// Fullscreen triangle — 3 vertices, no vertex buffer
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv - vec2<f32>(0.5);
    uv.x *= u.aspect_ratio;
    uv *= u.uv_scale;

    let tc = u32(i32(u.trigger_count));

    // Fill mode: 0 = Solid, 1 = Mixed, 2 = Wireframe
    // Mixed cycles 6 variants (3 shapes × 2 fills), matching original behavior
    var shape_idx: u32;
    var is_wireframe: bool;
    var rot_step: u32;

    if u.fill_mode < 0.5 {
        // Solid: cycle 3 shapes, always solid
        shape_idx = tc % 3u;
        is_wireframe = false;
        rot_step = (tc / 3u) % 8u;
    } else if u.fill_mode < 1.5 {
        // Mixed: cycle 6 variants (3 shapes × 2 fills), original snap behavior
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

    // Rotation: 4 angles × 2 directions = 8 steps
    let DEG45 = 0.78539816; // pi/4
    let target_angle = f32(rot_step % 4u) * DEG45;
    let rot_direction = select(1.0, -1.0, rot_step >= 4u);
    let rotation = target_angle * rot_direction;

    // Transform UV
    var p = uv / 0.315;
    p = rotate2d(p, rotation);

    // Evaluate SDF
    var d: f32;
    switch shape_idx {
        case 1u: { d = sd_diamond(p, 1.0); }
        case 2u: { d = sd_octagon(p, 1.0); }
        default: { d = sd_square(p, 1.0); }
    }

    // Anti-aliased edge
    let pw = fwidth(d);

    var shape: f32;
    if is_wireframe {
        let thickness = u.line_thickness;
        shape = 1.0 - smoothstep(thickness - pw, thickness + pw, abs(d));
    } else {
        shape = 1.0 - smoothstep(-pw, pw, d);
    }

    let lum = saturate(shape);

    return vec4<f32>(lum, lum, lum, lum);
}
