// node.apply_radial_burst_3d_to_particles — per-particle 3D injection
// burst around one of four hardcoded tetrahedron-vertex zones. Applies a
// noise-perturbed radial push + vortex-ring tangent directly to
// position.xyz. Bit-exact with the injection step of the legacy fused
// fluid_simulate_3d.
//
// inject_index < 0 disables the burst; 0..3 selects one of the four
// tetrahedron-vertex zones. The force envelope is attack(phase) *
// decay(phase); spatial falloff is a smooth quartic within radius 0.25.

struct Uniforms {
    active_count: u32,
    inject_index: i32,   // -1 = off
    inject_force: f32,
    inject_phase: f32,
    time2: f32,
    dt_scaled: f32,
    _pad0: u32,
    _pad1: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read_write> particles: array<Particle>;

// 3D injection zone points (tetrahedron vertices, matches fluid_simulate_3d).
const INJECT_POINTS_3D: array<vec3<f32>, 4> = array<vec3<f32>, 4>(
    vec3<f32>(0.644, 0.644, 0.644),
    vec3<f32>(0.644, 0.356, 0.356),
    vec3<f32>(0.356, 0.644, 0.356),
    vec3<f32>(0.356, 0.356, 0.644),
);
const INJECT_FORCE_RADIUS_3D: f32 = 0.25;

// SimplexNoise2D — same formulation as fluid_simulate_3d.wgsl.
const SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378;
    let G2: f32 = 0.21132486540;

    let s = (v.x + v.y) * F2;
    let i = floor(v + s);
    let t = (i.x + i.y) * G2;
    let x0 = v - (i - t);

    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    let x1 = x0 - i1 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;

    let h0 = wang_hash(u32(i.x + 10000.0) * 73856093u ^ u32(i.y + 10000.0) * 19349663u);
    let h1 = wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u ^ u32(i.y + 1.0 + 10000.0) * 19349663u);

    let g0 = vec2<f32>(SIMPLEX_GRAD2_X[h0 & 7u], SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(SIMPLEX_GRAD2_X[h1 & 7u], SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(SIMPLEX_GRAD2_X[h2 & 7u], SIMPLEX_GRAD2_Y[h2 & 7u]);

    let t0 = 0.5 - dot(x0, x0);
    let t1 = 0.5 - dot(x1, x1);
    let t2 = 0.5 - dot(x2, x2);

    let n0 = select(t0 * t0 * t0 * t0 * dot(g0, x0), 0.0, t0 < 0.0);
    let n1 = select(t1 * t1 * t1 * t1 * dot(g1, x1), 0.0, t1 < 0.0);
    let n2 = select(t2 * t2 * t2 * t2 * dot(g2, x2), 0.0, t2 < 0.0);

    return clamp((n0 + n1 + n2) * 35.0 + 0.5, 0.0, 1.0);
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    if u.inject_index < 0 {
        return;
    }
    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let ipos = p.position;
    let idx_inject = u32(u.inject_index);
    let delta = ipos - INJECT_POINTS_3D[idx_inject];
    let dist2 = dot(delta, delta);
    let force_r2 = INJECT_FORCE_RADIUS_3D * INJECT_FORCE_RADIUS_3D;

    // Force envelope: fast attack (~10% of burst), exponential decay.
    let attack = clamp(u.inject_phase * 10.0, 0.0, 1.0);
    let decay2 = exp(-u.inject_phase * 3.0);
    let envelope = attack * decay2;

    if dist2 < force_r2 && dist2 > 0.0001 && envelope > 0.001 {
        let dist = sqrt(dist2);
        let t2 = dist / INJECT_FORCE_RADIUS_3D;
        let radial = delta / dist;

        // Spatial falloff: smooth quartic.
        let ff = (1.0 - t2 * t2);
        let falloff = ff * ff;

        // Noise perturbation: breaks spherical symmetry.
        let noise_angle1 = simplex_noise_2d(ipos.xy * 8.0 + vec2<f32>(u.time2 * 0.3)) * 6.28318;
        let noise_angle2 = simplex_noise_2d(ipos.yz * 8.0 + vec2<f32>(u.time2 * 0.3 + 50.0)) * 6.28318;
        let noise_dir = vec3<f32>(cos(noise_angle1), sin(noise_angle1) * cos(noise_angle2), sin(noise_angle2));
        let perturbed_radial = normalize(radial + noise_dir * 0.4 * t2);

        // Vortex ring via cross product.
        var tangent = normalize(cross(radial, vec3<f32>(0.0, 1.0, 0.0)));
        if length(cross(radial, vec3<f32>(0.0, 1.0, 0.0))) < 0.001 {
            tangent = normalize(cross(radial, vec3<f32>(1.0, 0.0, 0.0)));
        }
        let curl_profile = t2 * (1.0 - t2) * 4.0;
        let curl_force_v = tangent * curl_profile;

        let strength = u.inject_force * envelope * falloff * u.dt_scaled;
        let push = perturbed_radial * strength + curl_force_v * strength * 0.5;
        p.position = clamp(ipos + push, vec3<f32>(0.001), vec3<f32>(0.999));
        particles[i] = p;
    }
}
