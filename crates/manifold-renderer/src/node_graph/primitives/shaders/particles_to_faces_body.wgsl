// node.particles_to_faces — fusable BUFFER body, GATHER. One thread per
// padded cell p of the face grid ((nodes + 1) per axis); it owns the lower x,
// y and z faces, face a at p on axis a and p + ½ on the other two, in cells
// from the lattice minimum. Each face sums the tent weight
// Π max(0, 1 − |q − face|) of every live particle (radius > 0) in the
// 3 × 3 × 3 cells around p, q its position in cells, and the weighted
// velocity component along the face's normal; the velocity is their ratio,
// 0 where no particle reaches. Faces past the lattice give zeros. `sorted`
// (FluidParticle → Element) and `cell_ranges` (CellRange → Element2) are
// gathered; a lattice larger than `cell_ranges` gives zeros and a range past
// `sorted` is cut short. Output FaceSample (Element3).

fn body(
    idx: u32,
    count: u32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
) -> Element3 {
    var out = Element3(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let cells = u32(n.x) * u32(n.y) * u32(n.z);
    if cells > arrayLength(&buf_cell_ranges) || idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    var exists = vec3<bool>(false);
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        exists[a] = all(other < n);
    }
    if !any(exists) {
        return out;
    }
    let lo = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let inv_h = 1.0 / cell_size;
    let first = max(p - vec3<i32>(1), vec3<i32>(0));
    let last = min(p + vec3<i32>(1), n - vec3<i32>(1));
    let particles = arrayLength(&buf_sorted);
    var weight = vec3<f32>(0.0);
    var momentum = vec3<f32>(0.0);
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let range = buf_cell_ranges[u32(x + n.x * (y + n.y * z))];
                let start = min(range.start, particles);
                let end = start + min(range.count, particles - start);
                for (var s = start; s < end; s = s + 1u) {
                    let particle = buf_sorted[s];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let q = (particle.position_radius.xyz - lo) * inv_h;
                    for (var a = 0; a < 3; a = a + 1) {
                        if !exists[a] {
                            continue;
                        }
                        var face = vec3<f32>(p) + vec3<f32>(0.5);
                        face[a] = f32(p[a]);
                        let t = max(vec3<f32>(1.0) - abs(q - face), vec3<f32>(0.0));
                        let w = t.x * t.y * t.z;
                        weight[a] = weight[a] + w;
                        momentum[a] = momentum[a] + w * particle.velocity[a];
                    }
                }
            }
        }
    }
    let velocity = select(vec3<f32>(0.0), momentum / max(weight, vec3<f32>(1e-30)), weight > vec3<f32>(0.0));
    out.face_velocity = vec4<f32>(velocity, 0.0);
    out.face_weight = vec4<f32>(weight, 0.0);
    return out;
}
