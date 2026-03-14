struct Uniforms {
    time_val: f32,
    beat: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    pattern_type: f32,
    complexity: f32,
    contrast: f32,
    trigger_count: f32,
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

// ── Pattern 0: Classic — sum of sine waves ──
fn plasma_classic(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 3.0 + cx * 5.0;
    let v1 = sin(uv.x * freq + t * 1.3);
    let v2 = sin(uv.y * (freq * 0.8) + t * 0.9);
    let v3 = sin((uv.x + uv.y) * (freq * 0.6) + t * 1.1);
    let v4 = sin(length(uv * freq * 1.2) + t * 0.7);
    let angle = t * 0.3;
    let cs = cos(angle);
    let sn = sin(angle);
    let ruv = vec2<f32>(uv.x * cs - uv.y * sn, uv.x * sn + uv.y * cs);
    let v5 = sin(ruv.x * (freq * 1.4) + t * 1.5);
    return (v1 + v2 + v3 + v4 + v5) / 5.0;
}

// ── Pattern 1: Rings — concentric radial waves ──
fn plasma_rings(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 4.0 + cx * 8.0;
    let r = length(uv);
    let v1 = sin(r * freq - t * 2.0);
    let v2 = sin(r * freq * 1.5 + t * 1.3);
    let v3 = sin(atan2(uv.y, uv.x) * 3.0 + t * 0.7);
    return (v1 + v2 + v3) / 3.0;
}

// ── Pattern 2: Diamond — axis-aligned interference ──
fn plasma_diamond(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 4.0 + cx * 6.0;
    let v1 = sin(uv.x * freq + t);
    let v2 = sin(uv.y * freq + t * 1.2);
    let v3 = sin((abs(uv.x) + abs(uv.y)) * freq * 0.7 + t * 0.8);
    let v4 = sin((uv.x * uv.x + uv.y * uv.y) * freq * 0.3 + t * 1.5);
    return (v1 + v2 + v3 + v4) / 4.0;
}

// ── Pattern 3: Warp — domain-warped sine field ──
fn plasma_warp(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 3.0 + cx * 4.0;
    var p = uv * freq;
    p.x += sin(p.y * 0.5 + t) * (1.0 + cx);
    p.y += cos(p.x * 0.5 + t * 0.7) * (1.0 + cx);
    let v1 = sin(p.x + t * 0.9);
    let v2 = sin(p.y + t * 1.1);
    let v3 = sin((p.x + p.y) * 0.7 + t * 0.5);
    return (v1 + v2 + v3) / 3.0;
}

// ── Pattern 4: Cells — voronoi-like from sine interference ──
fn plasma_cells(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 3.0 + cx * 5.0;
    let v1 = sin(uv.x * freq + sin(uv.y * freq * 0.5 + t));
    let v2 = sin(uv.y * freq + sin(uv.x * freq * 0.5 + t * 0.8));
    let v3 = sin(length(uv + vec2<f32>(sin(t * 0.3), cos(t * 0.4))) * freq * 1.5);
    let v4 = sin(length(uv - vec2<f32>(sin(t * 0.5), cos(t * 0.2))) * freq * 1.2 + t);
    return (v1 * v2 + v3 + v4) / 3.0;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    var uv = in.uv - vec2<f32>(0.5);
    uv.x *= u.aspect_ratio;
    uv *= u.uv_scale;
    let t = u.time_val * u.anim_speed;
    let pattern = i32(u.pattern_type);
    let cx = u.complexity;

    var plasma: f32;
    switch pattern {
        case 1: { plasma = plasma_rings(uv, t, cx); }
        case 2: { plasma = plasma_diamond(uv, t, cx); }
        case 3: { plasma = plasma_warp(uv, t, cx); }
        case 4: { plasma = plasma_cells(uv, t, cx); }
        default: { plasma = plasma_classic(uv, t, cx); }
    }

    let edge = mix(0.3, 0.02, u.contrast);
    let lum = smoothstep(-edge, edge, plasma);
    return vec4<f32>(lum, lum, lum, lum);
}
