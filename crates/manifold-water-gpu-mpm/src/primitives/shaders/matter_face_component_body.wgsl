var<private> matter_pool_tiles: u32;

fn ts_pool_tiles() -> u32 { return matter_pool_tiles; }

fn ts_table_entries() -> u32 {
    let n = 4u * max(matter_pool_tiles, 1u);
    return 1u << (32u - countLeadingZeros(n - 1u));
}

fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.matter_face_component — fusable BUFFER body, GATHER. One thread per
// face of the seam's array for `axis`: (n+1) along the axis by n on the
// other two, x fastest, n = nodes − 7 cells. Face f sits at lattice node
// f + pad along the axis and f + pad + ½ on the other two, so it reads the
// four nodes around that centre: velocity_mass.xyz is the node's velocity,
// w its mass. The mean over the nodes with mass; 0 when none have any.
// Lattice node q is the world node round(lattice_min / cell_size) + q, read
// from the pooled `buf_grid` through the world-tile table (`buf_tiles`,
// `pool_tiles` tiles); a node in a tile the table does not hold reads the
// canonical node. A grid shorter than the pool gives zeros.

fn body(idx: u32, count: u32, axis: u32, nodes_x: i32, nodes_y: i32, nodes_z: i32, lattice_min_x: f32, lattice_min_y: f32, lattice_min_z: f32, cell_size: f32, pool_tiles: i32) -> f32 {
    matter_pool_tiles = u32(pool_tiles);
    let pad = 3;
    let nodes = vec3<i32>(nodes_x, nodes_y, nodes_z);
    let n = nodes - vec3<i32>(1 + 2 * pad);
    if axis > 2u || any(n < vec3<i32>(1)) || !(cell_size > 0.0)
        || arrayLength(&buf_grid) < ts_pool_tiles() * 512u + 1u {
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
    ) + vec3<i32>(pad);
    let origin = vec3<i32>(round(vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z) / cell_size));
    let b = (a + 1) % 3;
    let c = (a + 2) % 3;
    var sum = 0.0;
    var hits = 0.0;
    for (var db = 0; db < 2; db = db + 1) {
        for (var dc = 0; dc < 2; dc = dc + 1) {
            var q = f;
            q[b] = f[b] + db;
            q[c] = f[c] + dc;
            let node = buf_grid[ts_cell_at(origin + q)].velocity_mass;
            if node.w > 0.0 {
                sum = sum + node[a];
                hits = hits + 1.0;
            }
        }
    }
    return select(0.0, sum / max(hits, 1.0), hits > 0.0);
}
