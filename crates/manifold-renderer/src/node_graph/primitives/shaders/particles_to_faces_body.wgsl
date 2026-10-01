// node.particles_to_faces — fusable BUFFER body, GATHER. One thread per
// padded cell p of the face grid ((nodes + 1) per axis); it owns the lower x,
// y and z faces, face a at p on axis a and p + ½ on the other two, in cells
// from the lattice minimum. Each face sums the engine's Wyvill weight
// 1 − (4/9)·s³/r⁶ + (17/9)·s²/r⁴ − (22/9)·s/r² for s = |q − face|² < r², r = √3/2
// cells (half a cell's diagonal), over every live particle (radius > 0) in
// the 3 × 3 × 3 cells around p, q its position in cells, and the weighted
// velocity component along the face's normal. A face within r of q along
// its normal lies in q's cell or the one below, and along the other axes in
// q's cell or a neighbour, so the 27 cells hold every particle that reaches
// it. A face whose weight is over 1e-6 gets the ratio; any other face gets
// velocity 0 and weight 0, so node.extend_faces fills it. A box wall face
// (index 0 or nodes along its axis) keeps only the part of that velocity
// leaving the wall and is always valid (weight 1), so node.extend_faces
// never writes it: the separating wall the whole step uses. Faces past the
// lattice give zeros. `sorted` (FluidParticle → Element) and `cell_ranges`
// (CellRange → Element2) are gathered; a lattice larger than `cell_ranges`
// gives zeros and a range past `sorted` is cut short. Output FaceSample
// (Element3).
//
// Ported from FLIP Fluids velocityadvector.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md

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
    let rsq = 0.75;
    let coef1 = (4.0 / 9.0) / (rsq * rsq * rsq);
    let coef2 = (17.0 / 9.0) / (rsq * rsq);
    let coef3 = (22.0 / 9.0) / rsq;
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
                        let v = face - q;
                        let d2 = dot(v, v);
                        if !(d2 < rsq) {
                            continue;
                        }
                        let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                        weight[a] = weight[a] + w;
                        momentum[a] = momentum[a] + w * particle.velocity[a];
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
            velocity[a] = particles_to_faces_wall(velocity[a], p[a] == 0);
            weight[a] = 1.0;
        }
    }
    out.face_velocity = vec4<f32>(velocity, 0.0);
    out.face_weight = vec4<f32>(weight, 0.0);
    return out;
}

// A box wall lets water leave and never enter: of the velocity on a wall
// face it keeps only the part pointing into the box (up from the floor face,
// index 0; down from the lid face, index nodes).
fn particles_to_faces_wall(v: f32, low: bool) -> f32 {
    return select(min(v, 0.0), max(v, 0.0), low);
}
