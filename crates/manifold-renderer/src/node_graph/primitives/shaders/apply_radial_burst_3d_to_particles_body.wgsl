// node.apply_radial_burst_3d_to_particles — fusable BUFFER body (freeze §12,
// buffer domain), COINCIDENT. Per-particle 3D injection burst around one of four
// hardcoded tetrahedron-vertex zones (inject_index < 0 = off): noise-perturbed
// radial push + vortex-ring tangent on position.xyz. Matches
// apply_radial_burst_3d_to_particles.wgsl bit-for-bit (bespoke simplex + zone
// consts inlined, prefixed arb3_ for fusion-collision safety).
//
// ABI (buffer standalone codegen): `in` (Particle) coincident → e_in; in/out
// alias one buffer (run() binds it to slots 1 and 2), so returning e_in
// unchanged on any early-out reproduces the hand kernel's conditional write.
// `time2` (= seconds) and `dt_scaled` (= delta*60) are TWO DERIVED uniforms.
// Element = the Particle struct. The hand uses a local `t2` distinct from the
// derived `time2` — kept distinct here too.
const ARB3_INJECT_POINTS_3D: array<vec3<f32>, 4> = array<vec3<f32>, 4>(
    vec3<f32>(0.644, 0.644, 0.644),
    vec3<f32>(0.644, 0.356, 0.356),
    vec3<f32>(0.356, 0.644, 0.356),
    vec3<f32>(0.356, 0.356, 0.644),
);
const ARB3_INJECT_FORCE_RADIUS_3D: f32 = 0.25;

const ARB3_SIMPLEX_GRAD2_X: array<f32, 8> = array<f32, 8>(
     1.0,  0.7071,  0.0, -0.7071,
    -1.0, -0.7071,  0.0,  0.7071
);
const ARB3_SIMPLEX_GRAD2_Y: array<f32, 8> = array<f32, 8>(
     0.0,  0.7071,  1.0,  0.7071,
     0.0, -0.7071, -1.0, -0.7071
);

fn arb3_wang_hash(seed_in: u32) -> u32 {
    var seed = seed_in;
    seed = (seed ^ 61u) ^ (seed >> 16u);
    seed = seed * 9u;
    seed = seed ^ (seed >> 4u);
    seed = seed * 0x27d4eb2du;
    seed = seed ^ (seed >> 15u);
    return seed;
}

fn arb3_simplex_noise_2d(v: vec2<f32>) -> f32 {
    let F2: f32 = 0.36602540378;
    let G2: f32 = 0.21132486540;

    let s = (v.x + v.y) * F2;
    let i = floor(v + s);
    let t = (i.x + i.y) * G2;
    let x0 = v - (i - t);

    let i1 = select(vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), x0.x > x0.y);
    let x1 = x0 - i1 + G2;
    let x2 = x0 - 1.0 + 2.0 * G2;

    let h0 = arb3_wang_hash(u32(i.x + 10000.0) * 73856093u ^ u32(i.y + 10000.0) * 19349663u);
    let h1 = arb3_wang_hash(u32(i.x + i1.x + 10000.0) * 73856093u ^ u32(i.y + i1.y + 10000.0) * 19349663u);
    let h2 = arb3_wang_hash(u32(i.x + 1.0 + 10000.0) * 73856093u ^ u32(i.y + 1.0 + 10000.0) * 19349663u);

    let g0 = vec2<f32>(ARB3_SIMPLEX_GRAD2_X[h0 & 7u], ARB3_SIMPLEX_GRAD2_Y[h0 & 7u]);
    let g1 = vec2<f32>(ARB3_SIMPLEX_GRAD2_X[h1 & 7u], ARB3_SIMPLEX_GRAD2_Y[h1 & 7u]);
    let g2 = vec2<f32>(ARB3_SIMPLEX_GRAD2_X[h2 & 7u], ARB3_SIMPLEX_GRAD2_Y[h2 & 7u]);

    let t0 = 0.5 - dot(x0, x0);
    let t1 = 0.5 - dot(x1, x1);
    let t2 = 0.5 - dot(x2, x2);

    let n0 = select(t0 * t0 * t0 * t0 * dot(g0, x0), 0.0, t0 < 0.0);
    let n1 = select(t1 * t1 * t1 * t1 * dot(g1, x1), 0.0, t1 < 0.0);
    let n2 = select(t2 * t2 * t2 * t2 * dot(g2, x2), 0.0, t2 < 0.0);

    return clamp((n0 + n1 + n2) * 35.0 + 0.5, 0.0, 1.0);
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    inject_index: i32,
    inject_force: f32,
    inject_phase: f32,
    active_count: i32,
    time2: f32,
    dt_scaled: f32,
) -> Element {
    var p = e_in;
    if inject_index < 0 {
        return p;
    }
    if p.life <= 0.0 {
        return p;
    }

    let ipos = p.position;
    let idx_inject = u32(inject_index);
    let delta = ipos - ARB3_INJECT_POINTS_3D[idx_inject];
    let dist2 = dot(delta, delta);
    let force_r2 = ARB3_INJECT_FORCE_RADIUS_3D * ARB3_INJECT_FORCE_RADIUS_3D;

    // Force envelope: fast attack (~10% of burst), exponential decay.
    let attack = clamp(inject_phase * 10.0, 0.0, 1.0);
    let decay2 = exp(-inject_phase * 3.0);
    let envelope = attack * decay2;

    if dist2 < force_r2 && dist2 > 0.0001 && envelope > 0.001 {
        let dist = sqrt(dist2);
        let t2 = dist / ARB3_INJECT_FORCE_RADIUS_3D;
        let radial = delta / dist;

        // Spatial falloff: smooth quartic.
        let ff = (1.0 - t2 * t2);
        let falloff = ff * ff;

        // Noise perturbation: breaks spherical symmetry.
        let noise_angle1 = arb3_simplex_noise_2d(ipos.xy * 8.0 + vec2<f32>(time2 * 0.3)) * 6.28318;
        let noise_angle2 = arb3_simplex_noise_2d(ipos.yz * 8.0 + vec2<f32>(time2 * 0.3 + 50.0)) * 6.28318;
        let noise_dir = vec3<f32>(cos(noise_angle1), sin(noise_angle1) * cos(noise_angle2), sin(noise_angle2));
        let perturbed_radial = normalize(radial + noise_dir * 0.4 * t2);

        // Vortex ring via cross product.
        var tangent = normalize(cross(radial, vec3<f32>(0.0, 1.0, 0.0)));
        if length(cross(radial, vec3<f32>(0.0, 1.0, 0.0))) < 0.001 {
            tangent = normalize(cross(radial, vec3<f32>(1.0, 0.0, 0.0)));
        }
        let curl_profile = t2 * (1.0 - t2) * 4.0;
        let curl_force_v = tangent * curl_profile;

        let strength = inject_force * envelope * falloff * dt_scaled;
        let push = perturbed_radial * strength + curl_force_v * strength * 0.5;
        p.position = clamp(ipos + push, vec3<f32>(0.001), vec3<f32>(0.999));
    }

    return p;
}
