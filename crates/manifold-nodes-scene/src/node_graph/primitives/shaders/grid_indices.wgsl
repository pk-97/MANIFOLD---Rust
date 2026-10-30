// node.grid_indices: the triangle indices of a row-major vertex grid, in tiles of
// 8 x 8 quads (grid_indices.rs).

struct GridUniforms {
    cols: u32,
    rows: u32,
    count: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> u: GridUniforms;
@group(0) @binding(1) var<storage, read_write> indices: array<u32>;

const TILE: u32 = 8u;

// The q-th quad in tile order: tile rows of TILE quads tall, tiles TILE quads
// wide within a row, quads row-major within a tile. Only the last tile row
// and the last tile of a row are smaller, so the counts before them are
// whole tiles.
fn tiled_quad(q: u32, quads_x: u32, quads_y: u32) -> vec2<u32> {
    let tile_row = q / (quads_x * TILE);
    let in_row = q - tile_row * quads_x * TILE;
    let tall = min(TILE, quads_y - tile_row * TILE);
    let tile = in_row / (TILE * tall);
    let in_tile = in_row - tile * TILE * tall;
    let wide = min(TILE, quads_x - tile * TILE);
    return vec2<u32>(tile * TILE + in_tile % wide, tile_row * TILE + in_tile / wide);
}

// Quad (c, r) writes (c,r) (c,r+1) (c+1,r), then (c,r+1) (c+1,r+1) (c+1,r).
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.count {
        return;
    }
    let corner = i % 6u;
    let quad = tiled_quad(i / 6u, u.cols - 1u, u.rows - 1u);
    let col = quad.x + select(0u, 1u, corner == 2u || corner >= 4u);
    let row = quad.y + select(0u, 1u, corner == 1u || corner == 3u || corner == 4u);
    indices[i] = row * u.cols + col;
}
