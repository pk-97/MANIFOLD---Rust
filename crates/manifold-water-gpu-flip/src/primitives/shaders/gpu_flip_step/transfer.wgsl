// The affines beside `sorted`, the sort's companion of it.
@group(0) @binding(74) var<storage, read> affine_sorted: array<ParticleAffine>;

// The affine of the particle in sorted slot s: none where the slot's affine
// belongs to another particle (a birth in a slot an older particle left).
fn particle_affine(s: u32, id: u32) -> ParticleAffine {
    let affine = affine_sorted[s];
    if affine.id != id {
        return ParticleAffine(vec3<f32>(0.0), id, vec3<f32>(0.0), 0u, vec3<f32>(0.0), 0u);
    }
    return affine;
}

// The trilinear weight of a particle d cells from a face: the tent
// Π (1 − |d|) over the three axes, zero a cell or more away on any.
fn tent(d: vec3<f32>) -> f32 {
    let t = max(vec3<f32>(1.0) - abs(d), vec3<f32>(0.0));
    return t.x * t.y * t.z;
}

// 512 threads a slot of C, one per record. The APIC transfer (Jiang et al.
// 2015, as velocityadvector.cpp _advectionAPICProducerThread): each face
// sums, over every live particle in the 3 × 3 × 3 cells around p, the
// trilinear weight w = tent(face − q) and w · (v + C · (face − q) h), C the
// particle's affine row for the face's axis, so a particle carries its
// velocity's gradient onto the faces around it, not only its velocity. The
// tent's cell of reach on each side of the face lies inside those cells. A
// face over weight 1e-6 gets the ratio; any other gets velocity 0 and weight
// 0 for the extension to fill. A face in a wall's solid is gathered like any
// other: the pressure projection clears it and the solid constraint sets its
// velocity.
@compute @workgroup_size(256)
fn particles_to_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let p = ts_face_coord(idx);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let first = p - vec3<i32>(1);
    let last = p + vec3<i32>(1);
    let slots = u.capacity;
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
                    let c = particle_affine(s, particle.id);
                    let dx = face_x - q;
                    let wx = tent(dx);
                    if wx > 0.0 {
                        weight.x = weight.x + wx;
                        momentum.x = momentum.x + wx * (particle.velocity.x + dot(c.x, dx) * u.cell_size);
                    }
                    let dy = face_y - q;
                    let wy = tent(dy);
                    if wy > 0.0 {
                        weight.y = weight.y + wy;
                        momentum.y = momentum.y + wy * (particle.velocity.y + dot(c.y, dy) * u.cell_size);
                    }
                    let dz = face_z - q;
                    let wz = tent(dz);
                    if wz > 0.0 {
                        weight.z = weight.z + wz;
                        momentum.z = momentum.z + wz * (particle.velocity.z + dot(c.z, dz) * u.cell_size);
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
