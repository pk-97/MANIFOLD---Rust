// node.container_bounds_3d — fusable BUFFER body (freeze §12, buffer domain),
// COINCIDENT. Post-integration hard containment: toroidal wrap (None) or SDF
// reflect + clamp (Cube/Sphere/Torus). Matches container_bounds_3d.wgsl bit-for-
// bit (self-contained SDF helpers inlined, prefixed cb3_).
//
// ABI (buffer standalone codegen): `in` (Particle) coincident → e_in; in/out
// alias one buffer (run() binds it to slots 1 and 2), so returning e_in
// unchanged for a dead particle reproduces the hand kernel's early return.
// Element = the Particle struct. `container` is the Enum param (u32). No derived
// fields. `active_count` (the wrapper guard = dispatch_count) is unused here.
fn cb3_sd_box(p: vec3<f32>, half_size: vec3<f32>) -> f32 {
    let d = abs(p) - half_size;
    return length(max(d, vec3<f32>(0.0))) + min(max(d.x, max(d.y, d.z)), 0.0);
}

fn cb3_sd_sphere(p: vec3<f32>, r: f32) -> f32 {
    return length(p) - r;
}

fn cb3_sd_torus(p: vec3<f32>, big_r: f32, small_r: f32) -> f32 {
    let q = vec2<f32>(length(p.xz) - big_r, p.y);
    return length(q) - small_r;
}

fn cb3_container_sdf(p: vec3<f32>, ctype: u32, scale: f32) -> f32 {
    let centered = p - 0.5;
    switch ctype {
        case 1u: { return cb3_sd_box(centered, vec3<f32>(scale * 0.5)); }
        case 2u: { return cb3_sd_sphere(centered, scale * 0.5); }
        // tube 0.12 -> 0.18: 4.4x volume, hole kept — the
        // geometry half of the torus density normalization.
        case 3u: { return cb3_sd_torus(centered, scale * 0.3, scale * 0.18); }
        default: { return -1.0; }
    }
}

fn cb3_container_gradient(p: vec3<f32>, ctype: u32, scale: f32) -> vec3<f32> {
    let eps: f32 = 0.002;
    return normalize(vec3<f32>(
        cb3_container_sdf(p + vec3<f32>(eps, 0.0, 0.0), ctype, scale) - cb3_container_sdf(p - vec3<f32>(eps, 0.0, 0.0), ctype, scale),
        cb3_container_sdf(p + vec3<f32>(0.0, eps, 0.0), ctype, scale) - cb3_container_sdf(p - vec3<f32>(0.0, eps, 0.0), ctype, scale),
        cb3_container_sdf(p + vec3<f32>(0.0, 0.0, eps), ctype, scale) - cb3_container_sdf(p - vec3<f32>(0.0, 0.0, eps), ctype, scale),
    ));
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    container: u32,
    ctr_scale: f32,
    active_count: i32,
) -> Element {
    var p = e_in;
    if p.life <= 0.0 {
        return p;
    }

    var new_pos = p.position;
    if container == 0u {
        // No container: toroidal wrap on all 3 axes.
        new_pos = fract(new_pos + 1.0);
    } else {
        let d = cb3_container_sdf(new_pos, container, ctr_scale);
        if d > 0.0 {
            let normal = cb3_container_gradient(new_pos, container, ctr_scale);
            new_pos -= normal * (d + 0.001);
        }
        new_pos = clamp(new_pos, vec3<f32>(0.001), vec3<f32>(0.999));
    }

    p.position = new_pos;
    return p;
}
