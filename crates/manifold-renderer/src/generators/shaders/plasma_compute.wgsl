struct Uniforms {
    time_val: f32,
    aspect_ratio: f32,
    anim_speed: f32,
    uv_scale: f32,
    pattern_type: f32,
    complexity: f32,
    contrast: f32,
    trigger_count: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

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

// ── Pattern 1: Rings — asymmetric concentric radial waves ──
fn plasma_rings(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 4.0 + cx * 8.0;
    // Slowly rotating elliptical distortion breaks radial symmetry
    let angle = t * 0.2;
    let ca = cos(angle);
    let sa = sin(angle);
    let ruv = vec2<f32>(ca * uv.x - sa * uv.y, sa * uv.x + ca * uv.y);
    let r = length(vec2<f32>(ruv.x * 1.0, ruv.y * 1.6));
    let v1 = sin(r * freq - t * 2.0);
    let v2 = sin(r * freq * 1.5 + t * 1.3);
    // Off-center secondary ring avoids origin singularity
    let off = vec2<f32>(0.3 * sin(t * 0.4), 0.3 * cos(t * 0.5));
    let r2 = length(uv - off);
    let v3 = sin(r2 * freq * 0.8 + t * 0.7);
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

// ── Pattern 5: Noise — multi-octave sine noise ──
// Gentle frequency scaling (×1.8 per octave) keeps it smooth like other variants.
fn plasma_noise(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let octaves = 3u + u32(cx * 2.0);
    var val = 0.0;
    var amp = 1.0;
    var total_amp = 0.0;
    var freq = 2.5;
    var p = uv;
    for (var i = 0u; i < octaves; i++) {
        val += amp * sin(p.x * freq + t * 0.6 + sin(p.y * freq * 0.7 + t * 0.4));
        val += amp * sin(p.y * freq * 0.9 - t * 0.5 + cos(p.x * freq * 0.6 + t * 0.3));
        total_amp += amp * 2.0;
        amp *= 0.5;
        freq *= 1.8;
        let angle = 0.5 + t * 0.08;
        let cs = cos(angle);
        let sn = sin(angle);
        p = vec2<f32>(p.x * cs - p.y * sn, p.x * sn + p.y * cs);
    }
    return val / total_amp;
}

// ── Pattern 6: Fractal — self-similar sine stacks ──
// Smooth sine folding instead of abs() — avoids hard edges.
fn plasma_fractal(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    var p = uv * (2.0 + cx * 2.0);
    var val = 0.0;
    var scale = 1.0;
    var total_scale = 0.0;
    for (var i = 0; i < 5; i++) {
        let fi = f32(i);
        let v = sin(p.x + t * (0.4 + fi * 0.08)) *
                cos(p.y + t * (0.25 + fi * 0.1));
        val += v * scale;
        total_scale += scale;
        scale *= 0.55;
        // Smooth sine fold instead of abs() — no hard edges
        p = vec2<f32>(
            sin(p.x * 1.8 + t * 0.15 + fi),
            sin(p.y * 1.8 + t * 0.12 + fi * 0.7)
        );
    }
    return val / total_scale;
}

// ── Pattern 7: Lattice — grid interference ──
fn plasma_lattice(uv: vec2<f32>, t: f32, cx: f32) -> f32 {
    let freq = 3.0 + cx * 5.0;
    let v1 = sin(uv.x * freq * 3.14159 + t * 0.8) + sin(uv.y * freq * 3.14159 + t * 0.6);
    let v2 = sin((uv.x + uv.y) * freq * 2.22 + t * 1.1) + sin((uv.x - uv.y) * freq * 2.22 + t * 0.9);
    let warp_x = uv.x + sin(uv.y * 2.0 + t) * cx * 0.3;
    let warp_y = uv.y + cos(uv.x * 2.0 + t * 0.7) * cx * 0.3;
    let v3 = sin(warp_x * freq * 3.14159 + t * 0.5) + sin(warp_y * freq * 3.14159 - t * 0.3);
    return (v1 + v2 + v3) / 6.0;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv_raw = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var uv = uv_raw - vec2<f32>(0.5);
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
        case 5: { plasma = plasma_noise(uv, t, cx); }
        case 6: { plasma = plasma_fractal(uv, t, cx); }
        case 7: { plasma = plasma_lattice(uv, t, cx); }
        default: { plasma = plasma_classic(uv, t, cx); }
    }

    let edge = mix(0.3, 0.02, u.contrast);
    let lum = smoothstep(-edge, edge, plasma);
    let color = vec4<f32>(lum, lum, lum, lum);
    textureStore(output, vec2<i32>(id.xy), color);
}
