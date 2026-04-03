// Black Hole — Display Pass
//
// Samples the precomputed deflection map and the live particle density texture
// to compose the final image. Per-frame cost: one texture read per pixel.
//
// Deflection map (Rgba32Float):
//   R: final radius (0 = absorbed)
//   G: disk crossing radius (0 = no disk)
//   B: disk crossing angle (world-space atan2)
//   A: accumulated disk opacity

struct Uniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    aspect: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var deflection_map: texture_2d<f32>;
@group(0) @binding(2) var disk_density: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float, write>;

// Star field (same as deflection pass, for escaped rays)
fn star_field_from_angle(r: f32, seed_val: f32) -> vec3<f32> {
    // Use the final radius and a seed to generate star pattern
    let p = vec3<f32>(r * 10.0, seed_val * 400.0, r * seed_val * 7.0);
    let cell = floor(p);
    let f = fract(p) - 0.5;
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.985, h) * smoothstep(0.4, 0.0, length(f));
    let brightness = h * h * star * 0.3;
    return vec3<f32>(brightness);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5)
        / vec2<f32>(f32(dims.x), f32(dims.y));

    // Sample deflection map
    let defl = textureSampleLevel(deflection_map, s_linear, uv, 0.0);
    let final_r = defl.r;
    let disk_r = defl.g;
    let disk_angle = defl.b;
    let disk_opacity = defl.a;

    var color = vec3<f32>(0.0);

    // ── Event horizon (absorbed) ──
    if final_r < 0.5 && disk_opacity < 0.01 {
        // Pure black
        textureStore(output, gid.xy, vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
    }

    // ── Disk hit — sample particle density texture ──
    if disk_r > 0.1 {
        // Convert disk hit (r, angle) to density texture UV
        let angle_norm = (disk_angle + 3.14159265) / 6.28318530; // [0, 1]
        let r_norm = clamp(
            (disk_r - u.disk_inner) / (u.disk_outer - u.disk_inner),
            0.0, 1.0,
        );
        let disk_uv = vec2<f32>(angle_norm, r_norm);

        // Sample particle density (bilinear filtered)
        let density_sample = textureSampleLevel(disk_density, s_linear, disk_uv, 0.0);
        let particle_color = density_sample.rgb;
        let particle_density = density_sample.a;

        // If particles are present, use their color. Otherwise, use procedural fallback.
        if particle_density > 0.001 {
            color = particle_color * u.disk_glow;
        } else {
            // Procedural fallback for areas without particles
            let t = r_norm;
            let inner_col = vec3<f32>(1.0, 0.95, 0.85);
            let mid_col = vec3<f32>(1.0, 0.55, 0.15);
            let outer_col = vec3<f32>(0.6, 0.12, 0.02);
            var fallback: vec3<f32>;
            if t < 0.5 {
                fallback = mix(inner_col, mid_col, t * 2.0);
            } else {
                fallback = mix(mid_col, outer_col, (t - 0.5) * 2.0);
            }
            let intensity = u.disk_glow * (u.disk_inner * u.disk_inner) / (disk_r * disk_r);
            let swirl = 0.7 + 0.3 * sin(disk_angle * 8.0 + disk_r * 1.5 - u.time_val * 0.4);
            color = fallback * intensity * swirl * 0.3; // Dim fallback
        }

        color *= disk_opacity;
    }

    // ── Background stars (escaped rays) ──
    if final_r > 1.0 {
        let star_brightness = star_field_from_angle(final_r, disk_angle + uv.x * 100.0);
        color += star_brightness * (1.0 - disk_opacity);
    }

    // ── Photon ring glow ──
    if final_r > 1.0 && final_r < 5.0 {
        let ring_glow = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.3;
        color += vec3<f32>(0.7, 0.8, 1.0) * ring_glow * (1.0 - disk_opacity);
    }

    // ACES tone mapping
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    color = clamp(
        (color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0),
    );

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
