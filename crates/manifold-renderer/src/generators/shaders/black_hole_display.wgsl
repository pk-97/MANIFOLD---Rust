// Black Hole — Display Pass
//
// Composites: deflection map (lensing geometry) + screen-space particle density.
//
// Deflection map (Rgba32Float): R=final_r, G=disk_r, B=disk_angle, A=disk_opacity
// Particle density (Rgba16Float): R=density at this screen pixel

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

// Star field
fn star_field(seed1: f32, seed2: f32) -> vec3<f32> {
    let p = vec3<f32>(seed1 * 400.0, seed2 * 400.0, seed1 * seed2 * 200.0);
    let cell = floor(p);
    let f = fract(p) - 0.5;
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.985, h) * smoothstep(0.4, 0.0, length(f));
    let brightness = h * h * star * 0.3;
    let tint = vec3<f32>(
        0.8 + 0.2 * fract(h * 13.7),
        0.8 + 0.2 * fract(h * 27.3),
        0.9 + 0.1 * fract(h * 41.1),
    );
    return tint * brightness;
}

// Disk emission color from radius
fn disk_emission(disk_r: f32) -> vec3<f32> {
    let t = clamp((disk_r - u.disk_inner) / (u.disk_outer - u.disk_inner), 0.0, 1.0);
    let inner_col = vec3<f32>(1.0, 0.95, 0.85);
    let mid_col = vec3<f32>(1.0, 0.55, 0.15);
    let outer_col = vec3<f32>(0.6, 0.12, 0.02);
    if t < 0.5 {
        return mix(inner_col, mid_col, t * 2.0);
    }
    return mix(mid_col, outer_col, (t - 0.5) * 2.0);
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
    let disk_angle_raw = defl.b;
    let disk_opacity = defl.a;

    // Sample particle density (screen-space, same resolution)
    let particle_d = textureSampleLevel(particle_density, s_linear, uv, 0.0).r;

    var color = vec3<f32>(0.0);

    // ── Particles (screen-space projected) ──
    if particle_d > 0.001 {
        // Use deflection disk_r for coloring if available, otherwise estimate from density
        var emit_r = disk_r;
        if emit_r < 0.1 {
            // Particle visible but deflection says no disk hit at this pixel
            // (particle is in front of or behind the lensed disk plane)
            emit_r = u.disk_inner + (u.disk_outer - u.disk_inner) * 0.5;
        }
        let emit_col = disk_emission(emit_r);
        let clamped_density = min(particle_d, 2.0);
        color = emit_col * clamped_density * u.disk_glow;
    }

    // ── Deflection-based disk (gravitationally lensed view) ──
    if disk_r > 0.1 && particle_d < 0.01 {
        let disk_angle = disk_angle_raw + u.orbit_angle;
        let emit_col = disk_emission(disk_r);
        let intensity = u.disk_glow * (u.disk_inner * u.disk_inner) / (disk_r * disk_r);
        let swirl = 0.7 + 0.3 * sin(disk_angle * 8.0 + disk_r * 1.5 - u.time_val * 0.4);
        color = emit_col * intensity * swirl * 0.2 * disk_opacity;
    }

    // Stars
    if final_r > 1.0 {
        let star_alpha = max(1.0 - disk_opacity - particle_d, 0.0);
        color += star_field(final_r * 0.01, disk_angle_raw + uv.x * 50.0) * star_alpha;
    }

    // Photon ring
    if final_r > 1.0 && final_r < 5.0 {
        let ring_glow = exp(-(final_r - 1.5) * (final_r - 1.5) * 8.0) * 0.3;
        color += vec3<f32>(0.7, 0.8, 1.0) * ring_glow * max(1.0 - disk_opacity, 0.0);
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
