// Metallic Glass — Procedural HDR studio environment map generator.
//
// Generates a 512×256 equirectangular HDR environment map simulating
// a photography studio interior. Run once at init, sampled per-frame
// by the render shader.
//
// Features:
//   - Noise-textured walls and ceiling (not uniform)
//   - Rectangular overhead soft box panels (HDR bright)
//   - Narrow strip/tube lights (very bright, create chrome streaks)
//   - Dark floor with subtle reflection
//   - Equipment silhouettes (break up uniformity)
//   - Proper HDR range: ambient ~0.1, walls ~0.3, panels ~8, tubes ~60

@group(0) @binding(0) var dst_tex: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318530;

// ─── Simple hash-based noise ───────────────────────────────────────

fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);  // smoothstep

    let a = hash21(i);
    let b = hash21(i + vec2(1.0, 0.0));
    let c = hash21(i + vec2(0.0, 1.0));
    let d = hash21(i + vec2(1.0, 1.0));

    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var val = 0.0;
    var amp = 0.5;
    var pos = p;
    for (var i = 0; i < 4; i++) {
        val += value_noise(pos) * amp;
        pos *= 2.1;
        amp *= 0.5;
    }
    return val;
}

// ─── Studio environment features ───────────────────────────────────

// Soft rectangular light panel
fn rect_light(azimuth: f32, elevation: f32,
              az_center: f32, el_center: f32,
              az_width: f32, el_height: f32,
              intensity: f32) -> f32 {
    let az_dist = abs(azimuth - az_center);
    let el_dist = abs(elevation - el_center);
    let az_mask = smoothstep(az_width, az_width * 0.7, az_dist);
    let el_mask = smoothstep(el_height, el_height * 0.7, el_dist);
    return az_mask * el_mask * intensity;
}

// Narrow tube/strip light
fn strip_light(elevation: f32, el_center: f32, width: f32, intensity: f32) -> f32 {
    return exp(-pow((elevation - el_center) / width, 2.0)) * intensity;
}

// ─── Main ──────────────────────────────────────────────────────────

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let width = 512u;
    let height = 256u;
    if gid.x >= width || gid.y >= height { return; }

    // Equirectangular mapping: UV → spherical direction
    let u_coord = f32(gid.x) / f32(width);
    let v_coord = f32(gid.y) / f32(height);

    let azimuth = u_coord * TAU - PI;        // -π to π
    let elevation = v_coord * PI - PI * 0.5;  // -π/2 (floor) to π/2 (ceiling)

    // Noise coordinates for texturing
    let noise_uv = vec2<f32>(u_coord * 8.0, v_coord * 4.0);

    // ── Base: dark studio ambient ──
    var color = vec3<f32>(0.08, 0.08, 0.09);

    // ── Walls: mid-grey with noise texture ──
    // Walls are near the horizon (elevation ≈ 0)
    let wall_mask = exp(-3.0 * elevation * elevation);
    let wall_noise = fbm(noise_uv * 3.0) * 0.3 + 0.2;
    let wall_color = vec3(0.28, 0.27, 0.26) * wall_noise;
    color += wall_color * wall_mask;

    // ── Ceiling: lighter with panel texture ──
    let ceil_mask = smoothstep(0.3, 0.8, elevation);
    let ceil_noise = fbm(noise_uv * 2.0 + vec2(50.0, 0.0)) * 0.15 + 0.3;
    color += vec3(0.35, 0.34, 0.33) * ceil_noise * ceil_mask;

    // ── Floor: dark with subtle sheen ──
    let floor_mask = smoothstep(-0.2, -0.6, elevation);
    let floor_noise = fbm(noise_uv * 4.0 + vec2(0.0, 80.0)) * 0.08 + 0.04;
    color += vec3(0.05, 0.05, 0.06) * floor_noise * floor_mask;

    // ── Overhead soft box panels (key lights) ──
    // Main panel: large, overhead, slightly warm
    color += vec3(1.0, 0.95, 0.88) *
        rect_light(azimuth, elevation, 0.3, 1.1, 0.5, 0.25, 8.0);

    // Secondary panel: offset, slightly cooler
    color += vec3(0.9, 0.93, 1.0) *
        rect_light(azimuth, elevation, -1.8, 1.0, 0.4, 0.2, 5.0);

    // Small fill panel behind camera
    color += vec3(0.95, 0.95, 0.95) *
        rect_light(azimuth, elevation, 2.8, 0.9, 0.3, 0.15, 3.0);

    // ── Strip/tube lights (create the chrome streaks) ──
    // These are VERY bright and narrow — the key to metallic reflections
    let strip_az_mask1 = smoothstep(0.4, 0.0, abs(azimuth - 0.8));
    color += vec3(1.0, 0.97, 0.92) * strip_light(elevation, 0.7, 0.015, 60.0) * strip_az_mask1;

    let strip_az_mask2 = smoothstep(0.5, 0.0, abs(azimuth + 1.2));
    color += vec3(0.92, 0.95, 1.0) * strip_light(elevation, 0.85, 0.015, 45.0) * strip_az_mask2;

    // Lower accent strip (cool, creates underside reflections)
    let strip_az_mask3 = smoothstep(0.6, 0.0, abs(azimuth - 2.0));
    color += vec3(0.85, 0.9, 1.0) * strip_light(elevation, 0.15, 0.02, 30.0) * strip_az_mask3;

    // ── Spot highlights on walls (practical lights / reflectors) ──
    let spot1 = exp(-8.0 * (pow(azimuth - 1.5, 2.0) + pow(elevation - 0.2, 2.0)));
    color += vec3(2.0, 1.8, 1.5) * spot1;

    let spot2 = exp(-10.0 * (pow(azimuth + 0.5, 2.0) + pow(elevation + 0.1, 2.0)));
    color += vec3(1.5, 1.7, 2.0) * spot2;

    let spot3 = exp(-12.0 * (pow(azimuth + 2.5, 2.0) + pow(elevation - 0.35, 2.0)));
    color += vec3(1.8, 1.6, 1.4) * spot3;

    // ── Equipment silhouettes (break uniformity) ──
    // Dark shapes near the horizon that block wall light
    let equip1 = smoothstep(0.08, 0.0, abs(azimuth - 0.0)) *
                 smoothstep(0.0, -0.15, elevation) * smoothstep(-0.5, -0.15, elevation);
    color *= 1.0 - equip1 * 0.7;

    let equip2 = smoothstep(0.06, 0.0, abs(azimuth - 1.8)) *
                 smoothstep(0.1, -0.1, elevation) * smoothstep(-0.4, -0.1, elevation);
    color *= 1.0 - equip2 * 0.6;

    // ── High-frequency noise overlay (micro-detail) ──
    let detail = value_noise(noise_uv * 20.0) * 0.06 + 0.97;
    color *= detail;

    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
