// Dense grid indexing, x fastest, then y, then z.
// Needs nothing from the including shader.

// Cells per tile side, the same as `TILE` in `tile_storage.rs`.
const TS_TILE: u32 = 8u;
// No cell: a list thread past the lattice, or a neighbour outside it.
const TS_NONE: u32 = 0xffffffffu;

fn ts_grid_index(p: vec3<i32>, dims: vec3<i32>) -> u32 {
    return u32(p.x + dims.x * (p.y + dims.y * p.z));
}

fn ts_grid_coord(idx: u32, dims: vec3<i32>) -> vec3<i32> {
    return vec3<i32>(
        i32(idx % u32(dims.x)),
        i32((idx / u32(dims.x)) % u32(dims.y)),
        i32(idx / (u32(dims.x) * u32(dims.y))),
    );
}

// Matter's node lattice: node-centred with its padding nodes (matter_walls.wgsl),
// `nodes` the padded counts. Dense today; the tile pool later replaces these
// bodies, not their callers.
fn ts_node_at(p: vec3<i32>, nodes: vec3<i32>) -> u32 {
    return ts_grid_index(p, nodes);
}

fn ts_node_coord(idx: u32, nodes: vec3<i32>) -> vec3<i32> {
    return ts_grid_coord(idx, nodes);
}
