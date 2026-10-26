// One thread per face record. Each face sums the engine's Wyvill weight
// 1 − (4/9)·s³/r⁶ + (17/9)·s²/r⁴ − (22/9)·s/r² for s = |q − face|² < r²,
// r = √3/2 cells, over every live particle in the 3 × 3 × 3 cells around p,
// and the weighted velocity along its normal. A face over weight 1e-6 gets
// the ratio; any other gets velocity 0 and weight 0 for the extension to
// fill. A box wall face is closed: velocity 0, valid (weight 1), so the
// extension never writes it (the engine's domain boundary, weight 0 in the
// solve, its velocity the static solid's).
@compute @workgroup_size(256)
fn particles_to_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = c_face_index(gid.x);
    if idx == NO_CELL {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    if u.narrow_band == 2u {
        let cell = clamp(p, vec3<i32>(0), n - vec3<i32>(1));
        if !narrow_cell(flatten(cell, n)) {
            faces_out[idx] = canonical_face(p, n);
            return;
        }
    }
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    var exists = vec3<bool>(false);
    for (var a = 0; a < 3; a = a + 1) {
        exists[a] = face_exists(p, n, a);
    }
    if !any(exists) {
        faces_out[idx] = out;
        return;
    }
    let inv_h = 1.0 / u.cell_size;
    let first = max(p - vec3<i32>(1), vec3<i32>(0));
    let last = min(p + vec3<i32>(1), n - vec3<i32>(1));
    let slots = u.capacity;
    let rsq = 0.75;
    let coef1 = (4.0 / 9.0) / (rsq * rsq * rsq);
    let coef2 = (17.0 / 9.0) / (rsq * rsq);
    let coef3 = (22.0 / 9.0) / rsq;
    var face_x = vec3<f32>(p) + vec3<f32>(0.5);
    face_x.x = f32(p.x);
    var face_y = vec3<f32>(p) + vec3<f32>(0.5);
    face_y.y = f32(p.y);
    var face_z = vec3<f32>(p) + vec3<f32>(0.5);
    face_z.z = f32(p.z);
    var weight = vec3<f32>(0.0);
    var momentum = vec3<f32>(0.0);
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let range = ranges[flatten(vec3<i32>(x, y, z), n)];
                let start = min(range.start, slots);
                let end = start + min(range.count, slots - start);
                for (var s = start; s < end; s = s + 1u) {
                    let particle = sorted[s];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let q = (particle.position_radius.xyz - u.box_min) * inv_h;
                    if exists.x {
                        let v = face_x - q;
                        let d2 = dot(v, v);
                        if d2 < rsq {
                            let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                            weight.x = weight.x + w;
                            momentum.x = momentum.x + w * particle.velocity.x;
                        }
                    }
                    if exists.y {
                        let v = face_y - q;
                        let d2 = dot(v, v);
                        if d2 < rsq {
                            let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                            weight.y = weight.y + w;
                            momentum.y = momentum.y + w * particle.velocity.y;
                        }
                    }
                    if exists.z {
                        let v = face_z - q;
                        let d2 = dot(v, v);
                        if d2 < rsq {
                            let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                            weight.z = weight.z + w;
                            momentum.z = momentum.z + w * particle.velocity.z;
                        }
                    }
                }
            }
        }
    }
    let valid = weight > vec3<f32>(1e-6);
    var velocity = select(vec3<f32>(0.0), momentum / max(weight, vec3<f32>(1e-6)), valid);
    weight = select(vec3<f32>(0.0), weight, valid);
    for (var a = 0; a < 3; a = a + 1) {
        if exists[a] && (p[a] == 0 || p[a] == n[a]) {
            velocity[a] = 0.0;
            weight[a] = 1.0;
        }
    }
    out.face_velocity = vec4<f32>(velocity, 0.0);
    out.face_weight = vec4<f32>(weight, 0.0);
    faces_out[idx] = out;
}
