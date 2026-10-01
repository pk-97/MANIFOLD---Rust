// Ported from FLIP Fluids levelsetutils.cpp and meshlevelset.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.solid_faces — fusable BUFFER body, GATHER. One thread per padded cell
// of the face grid (node.particles_to_faces' layout): each face's open
// fraction from the solid distance at its four corners, FLIP Fluids'
// MeshLevelSet::_getFaceWeight and LevelsetUtils::fractionInside ported line
// by line, open = clamp(1 − inside, 0, 1) as
// FluidSimulation::_updateWeightGridThread takes it. `solid` is a node
// lattice, (nodes + 1) per axis from the box's lowest corner, gathered
// through buf_solid; a lattice longer than it gives zeros. Box wall faces
// and faces past the lattice are 0: the walls are closed. Velocity is 0.

// The fraction of the segment from a to b inside the solid (phi < 0).
fn solid_faces_segment(a: f32, b: f32) -> f32 {
    if a < 0.0 && b < 0.0 {
        return 1.0;
    }
    if a < 0.0 && b >= 0.0 {
        return a / (a - b);
    }
    if a >= 0.0 && b < 0.0 {
        return b / (b - a);
    }
    return 0.0;
}

fn solid_faces_cycle(l: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(l.y, l.z, l.w, l.x);
}

// The fraction of the square inside the solid, corners bottom-left,
// bottom-right, top-left, top-right.
fn solid_faces_inside(bl: f32, br: f32, tl: f32, tr: f32) -> f32 {
    let inside = select(0, 1, bl < 0.0) + select(0, 1, tl < 0.0) + select(0, 1, br < 0.0) + select(0, 1, tr < 0.0);
    var l = vec4<f32>(bl, br, tr, tl);
    if inside == 4 {
        return 1.0;
    }
    if inside == 3 {
        for (var r = 0; r < 4 && l.x < 0.0; r = r + 1) {
            l = solid_faces_cycle(l);
        }
        let side0 = 1.0 - solid_faces_segment(l.x, l.w);
        let side1 = 1.0 - solid_faces_segment(l.x, l.y);
        return 1.0 - 0.5 * side0 * side1;
    }
    if inside == 2 {
        for (var r = 0; r < 4 && (l.x >= 0.0 || !(l.y < 0.0 || l.z < 0.0)); r = r + 1) {
            l = solid_faces_cycle(l);
        }
        if l.y < 0.0 {
            let left = solid_faces_segment(l.x, l.w);
            let right = solid_faces_segment(l.y, l.z);
            return 0.5 * (left + right);
        }
        let middle = 0.25 * (l.x + l.y + l.z + l.w);
        if middle < 0.0 {
            let side1 = 1.0 - solid_faces_segment(l.x, l.w);
            let side3 = 1.0 - solid_faces_segment(l.z, l.w);
            let side2 = 1.0 - solid_faces_segment(l.z, l.y);
            let side0 = 1.0 - solid_faces_segment(l.x, l.y);
            return 1.0 - (0.5 * side1 * side3 + 0.5 * side0 * side2);
        }
        let side0 = solid_faces_segment(l.x, l.y);
        let side1 = solid_faces_segment(l.x, l.w);
        let side2 = solid_faces_segment(l.z, l.y);
        let side3 = solid_faces_segment(l.z, l.w);
        return 0.5 * side0 * side1 + 0.5 * side2 * side3;
    }
    if inside == 1 {
        for (var r = 0; r < 4 && l.x >= 0.0; r = r + 1) {
            l = solid_faces_cycle(l);
        }
        let side0 = solid_faces_segment(l.x, l.w);
        let side1 = solid_faces_segment(l.x, l.y);
        return 0.5 * side0 * side1;
    }
    return 0.0;
}

fn solid_faces_phi(q: vec3<i32>, m: vec3<i32>) -> f32 {
    return buf_solid[u32(q.x + m.x * (q.y + m.y * q.z))];
}

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32, box_offset: f32) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    let faces = u32(m.x) * u32(m.y) * u32(m.z);
    if idx >= faces || faces > arrayLength(&buf_solid) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    // A face on the interface at every corner takes the symmetric limit.
    let tolerance = 8.0 * 1.1920929e-7 * (cell_size * f32(max(n.x, max(n.y, n.z))) + box_offset);
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) || p[a] == 0 || p[a] == n[a] {
            continue;
        }
        // The engine's corner order: U (j, k), V (k, i), W (j, i), each
        // (0, 0), (1, 0), (0, 1), (1, 1) along its two cross axes.
        var b = 1;
        var c = 2;
        if a == 1 {
            b = 2;
            c = 0;
        } else if a == 2 {
            c = 0;
        }
        var e1 = vec3<i32>(0);
        var e2 = vec3<i32>(0);
        e1[b] = 1;
        e2[c] = 1;
        let c0 = solid_faces_phi(p, m);
        let c1 = solid_faces_phi(p + e1, m);
        let c2 = solid_faces_phi(p + e2, m);
        let c3 = solid_faces_phi(p + e1 + e2, m);
        var inside = solid_faces_inside(c0, c1, c2, c3);
        if abs(c0) <= tolerance && abs(c1) <= tolerance && abs(c2) <= tolerance && abs(c3) <= tolerance {
            inside = 0.5;
        }
        out.face_weight[a] = clamp(1.0 - inside, 0.0, 1.0);
    }
    return out;
}
