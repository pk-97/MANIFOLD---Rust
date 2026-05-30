// node.container_repel_force_3d — soft container-boundary repulsion
// added in place to an Array<[f32; 3]> force buffer.
//
// Bit-exact with the soft-boundary-repulsion step of the legacy fused
// fluid_simulate_3d (a pre-integration force contribution, distinct from
// the post-integration hard containment in node.container_bounds_3d):
//
//   if container > 0:
//     d = container_sdf(pos, container, ctr_scale)
//     if d > -0.1:                       // within margin of the wall
//       n = container_gradient(pos, ...) // outward normal
//       t = clamp((d + 0.1) / 0.1, 0, 1)
//       forces[i] -= n * (t * t * 0.15)  // gentle inward cushion
//
// container enum: 0 = None (no repulsion), 1 = Cube, 2 = Sphere,
// 3 = Torus. Matches fluid_simulate_3d's container SDF set.

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

struct ForceVec {
    x: f32,
    y: f32,
    z: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read_write> forces: array<ForceVec>;

// --- Container SDFs (matches fluid_simulate_3d.wgsl) ---
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
        case 3u: { return sd_torus(centered, scale * 0.3, scale * 0.12); }
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
    if u.container == 0u {
        return;
    }
    let p = particles[i];
    if p.life <= 0.0 {
        return;
    }

    let pos = p.position;
    let d = container_sdf(pos, u.container, u.ctr_scale);
    let margin: f32 = 0.1;
    if d > -margin {
        let n = container_gradient(pos, u.container, u.ctr_scale);
        let t = clamp((d + margin) / margin, 0.0, 1.0);
        let push = n * (t * t * 0.15);
        var f = forces[i];
        f.x = f.x - push.x;
        f.y = f.y - push.y;
        f.z = f.z - push.z;
        forces[i] = f;
    }
}
