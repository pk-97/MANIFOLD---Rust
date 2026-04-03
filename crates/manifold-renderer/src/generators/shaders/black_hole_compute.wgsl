// Black Hole — Schwarzschild geodesic raytracer
//
// All distances in units of Schwarzschild radius (rs = 1).
// Speed of light c = 1 (natural units).
//
// Geodesic equations (polar coords in the ray's orbital plane):
//   φ̈ = -2/r · ṙ · φ̇
//   r̈ = 0.5/r² + r · φ̇²

struct Uniforms {
    time_val: f32,
    aspect: f32,
    speed: f32,
    cam_dist: f32,
    tilt_rad: f32,
    steps: f32,
    disk_inner: f32,
    disk_outer: f32,
    disk_glow: f32,
    uv_scale: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float, write>;

// ── Geodesic derivatives ──
// State: Y = (r, φ, ṙ, φ̇)
// Returns: dY/dt = (ṙ, φ̇, r̈, φ̈)
fn geodesic_deriv(r: f32, phi: f32, r_dot: f32, phi_dot: f32) -> vec4<f32> {
    let r2 = r * r;
    let r_ddot = 0.5 / r2 + r * phi_dot * phi_dot;
    let phi_ddot = -2.0 / r * r_dot * phi_dot;
    return vec4<f32>(r_dot, phi_dot, r_ddot, phi_ddot);
}

// ── RK4 integration step ──
fn rk4_step(state: vec4<f32>, h: f32) -> vec4<f32> {
    let k1 = geodesic_deriv(state.x, state.y, state.z, state.w);
    let s2 = state + 0.5 * h * k1;
    let k2 = geodesic_deriv(s2.x, s2.y, s2.z, s2.w);
    let s3 = state + 0.5 * h * k2;
    let k3 = geodesic_deriv(s3.x, s3.y, s3.z, s3.w);
    let s4 = state + h * k3;
    let k4 = geodesic_deriv(s4.x, s4.y, s4.z, s4.w);
    return state + (h / 6.0) * (k1 + 2.0 * k2 + 2.0 * k3 + k4);
}

// ── Accretion disk color ──
// Temperature gradient: hotter (white-yellow) near inner edge, cooler (orange-red) at outer
fn disk_color(r: f32, phi: f32) -> vec3<f32> {
    let t = (r - u.disk_inner) / (u.disk_outer - u.disk_inner);
    let t_clamped = clamp(t, 0.0, 1.0);

    // Inner = white-hot (1.0, 0.95, 0.8), outer = deep orange (0.8, 0.25, 0.05)
    let inner_col = vec3<f32>(1.0, 0.95, 0.8);
    let outer_col = vec3<f32>(0.8, 0.25, 0.05);
    var col = mix(inner_col, outer_col, t_clamped);

    // Radial falloff — intensity drops with r² (inverse square emission)
    let intensity = u.disk_glow * (u.disk_inner / r) * (u.disk_inner / r);

    // Swirl pattern for visual texture
    let swirl = 0.8 + 0.2 * sin(phi * 6.0 + r * 2.0 + u.time_val * 0.5);
    col *= intensity * swirl;

    return col;
}

// ── Star field background ──
fn star_field(dir: vec3<f32>) -> vec3<f32> {
    // Simple procedural stars from ray direction
    let p = dir * 500.0;
    let cell = floor(p);
    let f = fract(p) - 0.5;

    // Hash-based star placement
    let h = fract(sin(dot(cell, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
    let star = step(0.98, h) * smoothstep(0.5, 0.0, length(f));
    let brightness = h * h * star * 0.3;
    return vec3<f32>(brightness);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    // UV: centered, aspect-corrected, scaled
    let uv = (vec2<f32>(f32(gid.x), f32(gid.y)) + 0.5) / vec2<f32>(f32(dims.x), f32(dims.y));
    let ndc = (uv * 2.0 - 1.0) * u.uv_scale;
    let screen = vec2<f32>(ndc.x * u.aspect, -ndc.y);

    // ── Camera setup ──
    let orbit_angle = u.time_val * u.speed * 0.3;
    let cos_tilt = cos(u.tilt_rad);
    let sin_tilt = sin(u.tilt_rad);
    let cos_orbit = cos(orbit_angle);
    let sin_orbit = sin(orbit_angle);

    // Camera position: orbit at cam_dist, tilted
    let cam_pos = vec3<f32>(
        u.cam_dist * cos_tilt * cos_orbit,
        u.cam_dist * sin_tilt,
        u.cam_dist * cos_tilt * sin_orbit,
    );

    // Look-at matrix (target = origin)
    let fwd = normalize(-cam_pos);
    let world_up = vec3<f32>(0.0, 1.0, 0.0);
    let right = normalize(cross(fwd, world_up));
    let up = cross(right, fwd);

    // Ray direction (FOV ~60°)
    let fov_factor = 1.5;
    let ray_dir = normalize(fwd + screen.x * right * fov_factor + screen.y * up * fov_factor);

    // ── Convert to orbital plane coordinates ──
    // The ray travels in the plane defined by cam_pos and ray_dir.
    // We need initial (r, φ, ṙ, φ̇) in polar coords within that plane.

    let r0 = length(cam_pos);
    let phi0 = 0.0; // Start angle = 0 by convention (plane-local)

    // Decompose ray velocity into radial and tangential components
    let radial_dir = normalize(cam_pos); // points outward from BH
    let r_dot0 = dot(ray_dir, radial_dir); // radial velocity component

    // Tangential velocity: project ray_dir onto plane perpendicular to radial
    let tangential = ray_dir - r_dot0 * radial_dir;
    let phi_dot0 = length(tangential) / r0; // angular velocity = v_perp / r

    // Sign of φ̇: determine rotation direction
    // Use cross product to get consistent handedness
    let cross_test = cross(radial_dir, ray_dir);
    let tangent_ref = cross(radial_dir, cross_test);
    let phi_sign = sign(dot(tangential, normalize(tangent_ref + vec3<f32>(1e-10))));
    let phi_dot_signed = phi_dot0 * select(1.0, phi_sign, abs(phi_sign) > 0.01);

    // ── Determine disk crossing geometry ──
    // We need to track z-coordinate to detect accretion disk crossings (z = 0 plane).
    // In the orbital plane, reconstruct 3D position from (r, φ):
    //   pos_3d = r * (cos(φ) * e1 + sin(φ) * e2)
    // where e1 = radial_dir (initial), e2 = tangent direction in the orbital plane.
    let e1 = radial_dir;
    // e2 must be in the orbital plane and perpendicular to e1
    let orbit_normal = normalize(cross(cam_pos, ray_dir));
    let e2 = normalize(cross(orbit_normal, e1));

    // ── RK4 integration ──
    var state = vec4<f32>(r0, phi0, r_dot0, phi_dot_signed);
    let max_steps = i32(u.steps);
    // Adaptive step size: scale with initial r for stability
    let h = r0 * 0.002;

    var color = vec3<f32>(0.0);
    var prev_z = cam_pos.y; // Track z for disk crossing (use y as "up")
    var hit = false;

    for (var i = 0; i < max_steps; i++) {
        state = rk4_step(state, h);
        let r = state.x;
        let phi = state.y;

        // Event horizon check
        if r < 1.0 {
            color = vec3<f32>(0.0); // Absorbed
            hit = true;
            break;
        }

        // Escaped
        if r > 150.0 {
            // Reconstruct 3D direction for star field
            let pos_3d = r * (cos(phi) * e1 + sin(phi) * e2);
            let escaped_dir = normalize(pos_3d);
            color = star_field(escaped_dir);
            hit = true;
            break;
        }

        // Accretion disk crossing check
        // Reconstruct 3D position to check y-coordinate (disk is on y = 0 plane)
        let pos_3d = r * (cos(phi) * e1 + sin(phi) * e2);
        let cur_z = pos_3d.y;

        if prev_z * cur_z < 0.0 { // Sign change = crossed y = 0 plane
            if r > u.disk_inner && r < u.disk_outer {
                // Doppler-shifted disk color
                let doppler = 1.0 + 0.3 * sin(phi + u.time_val);
                color = disk_color(r, phi) * doppler;
                hit = true;
                break;
            }
        }
        prev_z = cur_z;
    }

    // If ray didn't terminate, treat as faint background
    if !hit {
        let pos_3d = state.x * (cos(state.y) * e1 + sin(state.y) * e2);
        color = star_field(normalize(pos_3d)) * 0.5;
    }

    // Gravitational lensing glow near event horizon
    // Faint emission ring at the photon sphere (r = 1.5 rs)
    let final_r = state.x;
    if !hit || final_r < 3.0 {
        let photon_ring = exp(-abs(final_r - 1.5) * 4.0) * 0.15;
        color += vec3<f32>(0.6, 0.7, 1.0) * photon_ring;
    }

    // Tone map (simple Reinhard)
    color = color / (color + vec3<f32>(1.0));

    textureStore(output, gid.xy, vec4<f32>(color, 1.0));
}
