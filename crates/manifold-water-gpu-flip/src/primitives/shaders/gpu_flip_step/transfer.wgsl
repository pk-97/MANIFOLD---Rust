// 512 threads a slot of C, one per record. Each face sums the engine's Wyvill weight
// 1 − (4/9)·s³/r⁶ + (17/9)·s²/r⁴ − (22/9)·s/r² for s = |q − face|² < r²,
// r = √3/2 cells, over every live particle in the 3 × 3 × 3 cells around p,
// and the weighted velocity along its normal. A face over weight 1e-6 gets
// the ratio; any other gets velocity 0 and weight 0 for the extension to
// fill. A face in a wall's solid is gathered like any other: the pressure
// projection clears it and the solid constraint sets its velocity.
@compute @workgroup_size(256)
fn particles_to_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let p = ts_face_coord(idx);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let first = p - vec3<i32>(1);
    let last = p + vec3<i32>(1);
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
                let range = cell_range(ts_cell_at(vec3<i32>(x, y, z)));
                let start = min(range.start, slots);
                let end = start + min(range.count, slots - start);
                for (var s = start; s < end; s = s + 1u) {
                    let particle = sorted[s];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let q = (particle.position_radius.xyz - u.origin) * u.inv_cell;
                    let vx = face_x - q;
                    let dx = dot(vx, vx);
                    if dx < rsq {
                        let w = 1.0 - coef1 * dx * dx * dx + coef2 * dx * dx - coef3 * dx;
                        weight.x = weight.x + w;
                        momentum.x = momentum.x + w * particle.velocity.x;
                    }
                    let vy = face_y - q;
                    let dy = dot(vy, vy);
                    if dy < rsq {
                        let w = 1.0 - coef1 * dy * dy * dy + coef2 * dy * dy - coef3 * dy;
                        weight.y = weight.y + w;
                        momentum.y = momentum.y + w * particle.velocity.y;
                    }
                    let vz = face_z - q;
                    let dz = dot(vz, vz);
                    if dz < rsq {
                        let w = 1.0 - coef1 * dz * dz * dz + coef2 * dz * dz - coef3 * dz;
                        weight.z = weight.z + w;
                        momentum.z = momentum.z + w * particle.velocity.z;
                    }
                }
            }
        }
    }
    let valid = weight > vec3<f32>(1e-6);
    let velocity = select(vec3<f32>(0.0), momentum / max(weight, vec3<f32>(1e-6)), valid);
    weight = select(vec3<f32>(0.0), weight, valid);
    out.face_velocity = vec4<f32>(velocity, 0.0);
    out.face_weight = vec4<f32>(weight, 0.0);
    faces_out[idx] = out;
}
