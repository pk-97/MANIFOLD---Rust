// node.container_bounds_3d — post-integration hard containment for 3D
// particles. The position-bounds policy atom: toroidal wrap (None) or
// SDF reflect + clamp (Cube / Sphere / Torus).
//
// Bit-exact (position-wise) with the containment step of the legacy
// fused fluid_simulate_3d:
//
//   if container == 0:        // None: toroidal wrap all 3 axes
//     pos = fract(pos + 1.0)
//   else:                     // SDF container
//     d = container_sdf(pos, container, ctr_scale)
//     if d > 0.0:             // escaped — push back inside along normal
//       n = container_gradient(pos, container, ctr_scale)
//       pos -= n * (d + 0.001)
//     pos = clamp(pos, 0.001, 0.999)
//
// The legacy kernel also wrote a reflected `p.velocity` on bounce, but
// nothing in the fluid sim ever reads particle velocity (each frame
// recomputes force from zero; the display splats positions). That write
// was dead state and is dropped — position behaviour is unchanged.

struct Uniforms {
    active_count: u32,
    container: u32,
    ctr_scale: f32,
    _pad0: u32,
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

fn sd_box(p: vec3<f32>, half_size: vec3<f32>) -> f32 {
    let d = abs(p) - half_size;
    return length(max(d, vec3<f32>(0.0))) + min(max(d.x, max(d.y, d.z)), 0.0);
}

fn sd_sphere(p: vec3<f32>, r: f32) -> f32 {
    return length(p) - r;
}

fn sd_torus(p: vec3<f32>, big_r: f32, small_r: f32) -> f32 {
    let q = vec2<f32>(length(p.xz) - big_r, p.y);
    return length(q) - small_r;
}

fn container_sdf(p: vec3<f32>, ctype: u32, scale: f32) -> f32 {
    let centered = p - 0.5;
    switch ctype {
        case 1u: { return sd_box(centered, vec3<f32>(scale * 0.5)); }
        case 2u: { return sd_sphere(centered, scale * 0.5); }
        // tube 0.12 -> 0.18 (2026-07-10): 4.4x volume, hole kept — the
        // geometry half of the torus density normalization.
        case 3u: { return sd_torus(centered, scale * 0.3, scale * 0.18); }
        default: { return -1.0; }
    }
}

fn container_gradient(p: vec3<f32>, ctype: u32, scale: f32) -> vec3<f32> {
    let eps: f32 = 0.002;
    return normalize(vec3<f32>(
        container_sdf(p + vec3<f32>(eps, 0.0, 0.0), ctype, scale) - container_sdf(p - vec3<f32>(eps, 0.0, 0.0), ctype, scale),
        container_sdf(p + vec3<f32>(0.0, eps, 0.0), ctype, scale) - container_sdf(p - vec3<f32>(0.0, eps, 0.0), ctype, scale),
        container_sdf(p + vec3<f32>(0.0, 0.0, eps), ctype, scale) - container_sdf(p - vec3<f32>(0.0, 0.0, eps), ctype, scale),
    ));
}

@compute @workgroup_size(256, 1, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    var p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    var new_pos = p.position;
    if u.container == 0u {
        // No container: toroidal wrap on all 3 axes.
        new_pos = fract(new_pos + 1.0);
    } else {
        let d = container_sdf(new_pos, u.container, u.ctr_scale);
        if d > 0.0 {
            // Escaped: push back inside along gradient (surface normal).
            let normal = container_gradient(new_pos, u.container, u.ctr_scale);
            new_pos -= normal * (d + 0.001);
        }
        // Safety clamp: ensure particle stays in [0.001, 0.999].
        new_pos = clamp(new_pos, vec3<f32>(0.001), vec3<f32>(0.999));
    }

    p.position = new_pos;
    particles[i] = p;
}
