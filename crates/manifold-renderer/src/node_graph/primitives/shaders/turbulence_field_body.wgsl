// FLIP Fluids turbulencefield.cpp:100-171 (MIT); THIRD_PARTY_NOTICES.md.
// The asymmetric loop and excluded final boundary index are intentional.
fn tf_face(c: vec3<i32>, axis: u32, pad: vec3<i32>, dims: vec3<u32>) -> f32 {
    let i = lf_face_index(c, axis, pad, dims);
    if i == LF_NONE { return 0.0; }
    if axis == 0u { return buf_face_u[i]; }
    if axis == 1u { return buf_face_v[i]; }
    return buf_face_w[i];
}

fn tf_velocity(c: vec3<i32>, pad: vec3<i32>, dims: vec3<u32>) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var a = 0u; a < 3u; a++) {
        var next = c; next[a]++;
        v[a] = 0.5 * (tf_face(c, a, pad, dims) + tf_face(next, a, pad, dims));
    }
    return v;
}

fn body(idx: u32, count: u32,
    face_cells_x: f32, face_cells_y: f32, face_cells_z: f32,
    nodes_x: f32, nodes_y: f32, nodes_z: f32, cell_size: f32,
) -> f32 {
    let cells = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z)) - vec3<u32>(1u);
    if idx >= cells.x * cells.y * cells.z || buf_distance[idx] >= 0.0 { return 0.0; }
    let c = vec3<i32>(i32(idx % cells.x), i32((idx / cells.x) % cells.y), i32(idx / (cells.x * cells.y)));
    let dims = vec3<u32>(vec3<f32>(face_cells_x, face_cells_y, face_cells_z));
    let pad = lf_pad(cells, dims);
    let vi = tf_velocity(c, pad, dims);
    let lo = max(c - vec3<i32>(2), vec3<i32>(0));
    let hi = min(c + vec3<i32>(2), vec3<i32>(cells) - vec3<i32>(1));
    var t = 0.0;
    for (var z = lo.z; z < hi.z; z++) {
        for (var y = lo.y; y < hi.y; y++) {
            for (var x = lo.x; x < hi.x; x++) {
                let n = vec3<i32>(x, y, z);
                let dv = vi - tf_velocity(n, pad, dims);
                let speed = length(dv);
                if speed < 1e-5 { continue; }
                let delta = vec3<f32>(c - n) * cell_size;
                let r = length(delta);
                t += speed * (1.0 - dot(dv / speed, delta / r)) * (1.0 - r / (sqrt(12.0) * cell_size));
            }
        }
    }
    return t;
}
