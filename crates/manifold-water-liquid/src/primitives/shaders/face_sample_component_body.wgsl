// node.face_sample_component — BUFFER body, GATHER. One thread per
// face of the seam's array for `axis`: (n+1) along the axis by n on the
// other two, x fastest. Face f of that array is component `axis` of padded
// cell f of the FaceSample lattice, read from the step's pooled faces through
// its slot table (`buf_tiles`, `pool_tiles` tiles); a face with weight 0
// reads 0. A pool shorter than the params' gives zeros. nodes_x/y/z are the
// padded lattice's (3 nodes of padding a side), so GPU FLIP has nodes − 4
// solver cells.

fn ts_cells() -> vec3<i32> {
    return max(vec3<i32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z)) - vec3<i32>(4), vec3<i32>(0));
}

fn ts_pool_tiles() -> u32 {
    return u32(params.pool_tiles);
}

fn ts_table_word(i: u32) -> u32 {
    return buf_tiles[i];
}

fn body(idx: u32, count: u32, axis: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, pool_tiles: f32) -> f32 {
    let n = ts_cells();
    if axis > 2u || ts_pool_len() > arrayLength(&buf_faces) {
        return 0.0;
    }
    let a = i32(axis);
    var dims = n;
    dims[a] = n[a] + 1;
    if idx >= u32(dims.x) * u32(dims.y) * u32(dims.z) {
        return 0.0;
    }
    let f = vec3<i32>(
        i32(idx % u32(dims.x)),
        i32((idx / u32(dims.x)) % u32(dims.y)),
        i32(idx / (u32(dims.x) * u32(dims.y))),
    );
    let s = buf_faces[ts_face(f)];
    return select(0.0, s.face_velocity[a], s.face_weight[a] > 0.0);
}
