struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    line_thickness: f32,
    uv_scale: f32,
    trigger_count: f32,
    _pad0: f32,
    _pad1: f32,
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

// ── SDF functions ──

fn sd_square(p: vec2<f32>) -> f32 {
    let d = abs(p) - vec2<f32>(0.35);
    return max(d.x, d.y);
}

fn sd_diamond(p: vec2<f32>) -> f32 {
    return abs(p.x) + abs(p.y) - 0.4;
}

fn sd_octagon(p: vec2<f32>) -> f32 {
    let ap = abs(p);
    let s = 0.4;
    // Regular octagon: max of axis-aligned and 45-degree distances
    let k = 0.41421356; // tan(pi/8)
    let d1 = max(ap.x, ap.y);
    let d2 = (ap.x + ap.y) * 0.70710678; // cos(pi/4)
    return max(d1, d2) - s;
}

// easeOutCubic: 1 - (1-t)^3
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - clamp(t, 0.0, 1.0);
    return 1.0 - inv * inv * inv;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Center and aspect-correct UV
    var uv = in.uv - vec2<f32>(0.5);
    uv.x *= u.aspect_ratio;
    uv *= u.uv_scale;

    // Beat-driven rotation with easeOutCubic
    let beat_frac = fract(u.beat);
    let rotation_angle = ease_out_cubic(beat_frac) * 1.5707963; // PI/2
    let cs = cos(rotation_angle);
    let sn = sin(rotation_angle);
    let rotated = vec2<f32>(uv.x * cs - uv.y * sn, uv.x * sn + uv.y * cs);

    // 6 variants: 3 shapes x 2 fill modes (solid, wireframe)
    // trigger_count % 6 selects variant
    let variant = i32(u.trigger_count) % 6;
    let shape_idx = variant / 2;    // 0=square, 1=diamond, 2=octagon
    let is_wireframe = (variant % 2) == 1;

    var d: f32;
    switch shape_idx {
        case 1: { d = sd_diamond(rotated); }
        case 2: { d = sd_octagon(rotated); }
        default: { d = sd_square(rotated); }
    }

    var lum: f32;
    if (is_wireframe) {
        // Wireframe: abs(d) < thickness
        let aa = fwidth(d);
        lum = 1.0 - smoothstep(u.line_thickness - aa, u.line_thickness + aa, abs(d));
    } else {
        // Solid fill
        let aa = fwidth(d);
        lum = 1.0 - smoothstep(-aa, aa, d);
    }

    return vec4<f32>(lum, lum, lum, lum);
}
