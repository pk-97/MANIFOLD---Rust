// node.container_repel_force_3d — fusable BUFFER body (freeze §12, buffer
// domain), COINCIDENT multi-input. Soft container-boundary repulsion added in
// place to a [f32;3] force buffer: within a 0.1 margin of the SDF wall, a gentle
// inward cushion `forces -= n * t*t*0.15`. Matches container_repel_force_3d.wgsl.
//
// ABI (buffer standalone codegen): TWO coincident array inputs — `in` (the
// [f32;3] force, FIRST → struct Element { x,y,z }) and `particles` (Particle,
// SECOND → struct Element2). `in` aliases `out` (run() binds the force buffer to
// both the read slot 1 and read_write slot 3; particles is binding 2). Returning
// e_in unchanged on an early-out reproduces the hand kernel's no-write. SDF
// helpers inlined (prefixed crf_). `container` is the Enum param (u32).
// `active_count` (= wrapper guard = dispatch_count) is unused here.
fn crf_sd_box(p: vec3<f32>, half_size: vec3<f32>) -> f32 {
    let d = abs(p) - half_size;
    return length(max(d, vec3<f32>(0.0))) + min(max(d.x, max(d.y, d.z)), 0.0);
}

fn crf_sd_sphere(p: vec3<f32>, r: f32) -> f32 {
    return length(p) - r;
}

fn crf_sd_torus(p: vec3<f32>, big_r: f32, small_r: f32) -> f32 {
    let q = vec2<f32>(length(p.xz) - big_r, p.y);
    return length(q) - small_r;
}

fn crf_container_sdf(p: vec3<f32>, ctype: u32, scale: f32) -> f32 {
    let centered = p - 0.5;
    switch ctype {
        case 1u: { return crf_sd_box(centered, vec3<f32>(scale * 0.5)); }
        case 2u: { return crf_sd_sphere(centered, scale * 0.5); }
        // tube 0.12 -> 0.18: 4.4x volume, hole kept — the
        // geometry half of the torus density normalization.
        case 3u: { return crf_sd_torus(centered, scale * 0.3, scale * 0.18); }
        default: { return -1.0; }
    }
}

fn crf_container_gradient(p: vec3<f32>, ctype: u32, scale: f32) -> vec3<f32> {
    let eps: f32 = 0.002;
    return normalize(vec3<f32>(
        crf_container_sdf(p + vec3<f32>(eps, 0.0, 0.0), ctype, scale) - crf_container_sdf(p - vec3<f32>(eps, 0.0, 0.0), ctype, scale),
        crf_container_sdf(p + vec3<f32>(0.0, eps, 0.0), ctype, scale) - crf_container_sdf(p - vec3<f32>(0.0, eps, 0.0), ctype, scale),
        crf_container_sdf(p + vec3<f32>(0.0, 0.0, eps), ctype, scale) - crf_container_sdf(p - vec3<f32>(0.0, 0.0, eps), ctype, scale),
    ));
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    e_particles: Element2,
    container: u32,
    ctr_scale: f32,
    active_count: i32,
) -> Element {
    if container == 0u {
        return e_in;
    }
    if e_particles.life <= 0.0 {
        return e_in;
    }

    let pos = e_particles.position;
    let d = crf_container_sdf(pos, container, ctr_scale);
    let margin: f32 = 0.1;
    var f = e_in;
    if d > -margin {
        let n = crf_container_gradient(pos, container, ctr_scale);
        let t = clamp((d + margin) / margin, 0.0, 1.0);
        let push = n * (t * t * 0.15);
        f.x = f.x - push.x;
        f.y = f.y - push.y;
        f.z = f.z - push.z;
    }
    return f;
}
