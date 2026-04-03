// Black Hole — Display Pass
//
// Composites deflection map + polar particle density.
// Particles are looked up via the deflection map's (disk_r, disk_angle),
// so they appear gravitationally lensed everywhere.

struct Uniforms {
    time_val: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    orbit_angle: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var deflection_map: texture_2d<f32>;
@group(0) @binding(2) var particle_density: texture_2d<f32>;
@group(0) @binding(3) var s_linear: sampler;
@group(0) @binding(4) var output: texture_storage_2d<rgba16float, write>;

fn star_field(seed1: f32, seed2: f32) -> vec3<f32> {
    let p = vec3<f32>(seed1 * 400.0, seed2 * 400.0, seed1 * seed2 * 200.0);
    let cell = floor(p);
    let f = fract(p) - 0.5;
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.985, h) * smoothstep(0.4, 0.0, length(f));
    let brightness = h * h * star * 0.3;
    return vec3<f32>(brightness) * vec3<f32>(
        0.8 + 0.2 * fract(h * 13.7),
        0.8 + 0.2 * fract(h * 27.3),
        0.9 + 0.1 * fract(h * 41.1),
    );
}

fn disk_emission(r: f32, angle: f32) -> vec3<f32> {
    let t = clamp((r - u.disk_inner) / (u.disk_outer - u.disk_inner), 0.0, 1.0);
    let inner_col = vec3<f32>(1.0, 0.95, 0.85);
    let mid_col = vec3<f32>(1.0, 0.55, 0.15);
    let outer_col = vec3<f32>(0.6, 0.12, 0.02);
    var col: vec3<f32>;
    if t < 0.5 {
        col = mix(inner_col, mid_col, t * 2.0);
    } else {
        col = mix(mid_col, outer_col, (t - 0.5) * 2.0);
    }

    // Radial intensity falloff
    let falloff = u.disk_glow * u.disk_inner / r;

    // Swirl texture
    let swirl = 0.75 + 0.25 * sin(angle * 12.0 + r * 2.0 - u.time_val * 0.3);

    return col * falloff * swirl;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    let uv = vec2<f32>(f32(gid.x) + 0.5, f32(gid.y) + 0.5)
        / vec2<f32>(f32(dims.x), f32(dims.y));

    let defl = textureSampleLevel(deflection_map, s_linear, uv, 0.0);
    let final_r = defl.r;
    let disk_r = defl.g;
    let disk_angle_raw = defl.b;
    let disk_opacity = defl.a;

    let disk_angle = disk_angle_raw + u.orbit_angle;

    var color = vec3<f32>(0.0);

    // ── Disk hit — look up particle density at lensed (r, angle) ──
    if disk_r > 0.1 {
        // Polar UV into particle density texture
        let angle_norm = fract((disk_angle + 3.14159265) / 6.28318530);
        let r_norm = clamp(
            (disk_r - u.disk_inner) / (u.disk_outer - u.disk_inner),
            0.0, 1.0,
        );
        let polar_uv = vec2<f32>(angle_norm, r_norm);

        let density = textureSampleLevel(particle_density, s_linear, polar_uv, 0.0).r;

        // Emission color from disk position
        let emit = disk_emission(disk_r, disk_angle);

        if density > 0.01 {
            // Particle-driven: density modulates emission
            color = emit * min(density, 3.0) * 1.5;
        } else {
            // Procedural fallback (dim, fills gaps)
            color = emit * 0.15;
        }

        color *= disk_opacity;
    }

    // Stars
    if final_r > 1.0 {
        color += star_field(final_r * 0.01, disk_angle_raw + uv.x * 50.0)
            * max(1.0 - disk_opacity, 0.0);
    }

    // Photon ring
    if final_r > 1.0 && final_r < 5.0 {
        let ring = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.25;
        color += vec3<f32>(0.7, 0.8, 1.0) * ring * max(1.0 - disk_opacity, 0.0);
    }

    // ACES
    let a = 2.51; let b = 0.03; let c = 2.43; let d = 0.59; let e = 0.14;
    color = clamp((color * (a * color + b)) / (color * (c * color + d) + e),
        vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
