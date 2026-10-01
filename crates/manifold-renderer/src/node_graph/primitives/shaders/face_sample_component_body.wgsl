// node.face_sample_component — fusable BUFFER body, GATHER. One thread per
// face of the seam's array for `axis`: (n+1) along the axis by n on the
// other two, x fastest. Face f of that array is component `axis` of padded
// cell f of the FaceSample lattice, (n+1)³ records x fastest; a face with
// weight 0 reads 0. `faces` is gathered through buf_faces; a lattice
// shorter than the params' gives zeros. nodes_x/y/z are the padded
// lattice's (3 nodes of padding a side), so the box has nodes − 7 cells.

fn body(idx: u32, count: u32, axis: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    let n = max(vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z)) - vec3<i32>(7), vec3<i32>(0));
    let m = n + vec3<i32>(1);
    if axis > 2u || u32(m.x) * u32(m.y) * u32(m.z) > arrayLength(&buf_faces) {
        return 0.0;
    }
    let a = i32(axis);
    var dims = n;
    dims[a] = m[a];
    if idx >= u32(dims.x) * u32(dims.y) * u32(dims.z) {
        return 0.0;
    }
    let f = vec3<i32>(
        i32(idx % u32(dims.x)),
        i32((idx / u32(dims.x)) % u32(dims.y)),
        i32(idx / (u32(dims.x) * u32(dims.y))),
    );
    let s = buf_faces[u32(f.x + m.x * (f.y + m.y * f.z))];
    return select(0.0, s.face_velocity[a], s.face_weight[a] > 0.0);
}
