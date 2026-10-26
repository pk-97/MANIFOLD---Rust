// ---- The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
// table)). Built every step from the sort's ranges, on the GPU, never read
// back; the parity and the lists live here so a replayed encode stays
// right. ----

// The cell passes' reach from a particle-holding cell.
const CELL_REACH: u32 = 2u;

fn tile_parity_word() -> u32 {
    return u.ring_max + 3u;
}

// The cell passes run over the cell set C, the first `tile_counts[0]` tiles
// of `tiles_by_ring`, through the triple at `tile_args[0]`.
fn c_cell_index(gid: u32) -> u32 {
    if u.narrow_band != 0u { return select(TS_NONE, gid, gid < ts_cell_count()); }
    return ts_list_cell(tiles_by_ring[gid >> 9u], gid);
}

// The gather runs over C's cells too: cell p owns face record p. A record
// past the lattice (p[a] == n[a]) belongs to no cell and is a constant: wall
// faces closed, the others absent (canonical_face), written by tiles_fill.
fn c_face_index(gid: u32) -> u32 {
    if u.narrow_band != 0u { return select(TS_NONE, gid, gid < ts_record_count()); }
    let cell = c_cell_index(gid);
    if cell == TS_NONE {
        return TS_NONE;
    }
    return ts_face(ts_cell_coord(cell));
}

// The gather's output at record p with no particle within reach: every face
// absent (velocity 0, weight 0) except a box wall face, closed (0, weight 1).
fn canonical_face(p: vec3<i32>, n: vec3<i32>) -> FaceSample {
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    for (var a = 0; a < 3; a = a + 1) {
        if face_exists(p, n, a) && (p[a] == 0 || p[a] == n[a]) {
            out.face_weight[a] = 1.0;
        }
    }
    return out;
}

// The canonical cell values: water 0, φ 3h (particle_distance's start, kept
// where no particle is within its scan), rhs 0.
fn canonical_cell(idx: u32) {
    tile_water[idx] = 0.0;
    tile_phi[idx] = 3.0 * u.cell_size;
    tile_rhs[idx] = 0.0;
}

// One thread per face record, once per lattice: every record and cell
// canonical, so the cells the C passes never visit read as the dense passes
// would leave them.
@compute @workgroup_size(256)
fn tiles_fill(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= ts_record_count() {
        return;
    }
    let n = ts_cells();
    let p = ts_face_coord(idx);
    faces_out[idx] = canonical_face(p, n);
    if all(p < n) {
        canonical_cell(ts_cell_at(p));
    }
}

// 512 threads a retired tile (the triple at `tile_args[3 · (ring_max + 1)]`):
// its cells and their records back to canonical, so a tile leaving C reads
// as the dense passes would leave it. Wall records past the lattice are
// never written by the C passes and keep their fill.
@compute @workgroup_size(256)
fn tiles_retire(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = ts_list_cell(tiles_retired[gid.x >> 9u], gid.x);
    if idx == TS_NONE {
        return;
    }
    let n = ts_cells();
    let p = ts_cell_coord(idx);
    canonical_cell(idx);
    faces_out[ts_face(p)] = canonical_face(p, n);
}

// Test-only (gpu_flip_tile_tests.rs): one thread per cell, after
// tiles_retire; NaN into the water, φ and rhs of every cell of a tile with
// ring > 1 (rank >= 2), so a dense read that escapes the defined-value rule
// shows in the particles or the faces.
@compute @workgroup_size(256)
fn poison_inactive(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= ts_cell_count() {
        return;
    }
    let tile = ts_grid_index(ts_cell_coord(idx) / i32(TS_TILE), ts_tile_dims());
    let half = tile_counts[tile_parity_word()] * ts_tile_count();
    if tile_rank[half + tile] < 2u {
        return;
    }
    let nan = bitcast<f32>(0x7fc00000u);
    tile_water[idx] = nan;
    tile_phi[idx] = nan;
    tile_rhs[idx] = nan;
}

var<workgroup> tile_near_min: array<u32, 256>;

// 32 lanes per tile, the sort's bin counts to `tile_near`: the Chebyshev
// cell distance from the tile's box to the nearest particle-holding cell,
// scanning the box grown by CELL_REACH (12³ cells at most); CELL_REACH + 1
// when none. Thread 0 flips the ring halves' parity for this step first: no
// other thread of this pass reads it, and `tiles_rings` runs after. An
// inactive clock slot runs none of the table's passes, so the next active
// step's retire measures against the last active step's C.
@compute @workgroup_size(256)
fn tiles_classify(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !clock_active() { return; }
    let t = gid.x >> 5u;
    let lane = gid.x & 31u;
    let valid = t < ts_tile_count();
    if gid.x == 0u {
        tile_counts[tile_parity_word()] = 1u - tile_counts[tile_parity_word()];
    }
    var near = CELL_REACH + 1u;
    if valid && u.all_tiles != 0u {
        near = 0u;
    } else if valid {
        let n = ts_cells();
        let reach = i32(CELL_REACH);
        let origin = ts_grid_coord(t, ts_tile_dims()) * i32(TS_TILE);
        let box_last = min(origin + vec3<i32>(i32(TS_TILE) - 1), n - vec3<i32>(1));
        let side = TS_TILE + 2u * CELL_REACH;
        for (var k = lane; k < side * side * side && near != 0u; k = k + 32u) {
            let c = origin - vec3<i32>(reach) + ts_grid_coord(k, vec3<i32>(i32(side)));
            if all(c >= vec3<i32>(0)) && all(c < n) {
                if ranges[ts_cell_at(c)].count != 0u {
                    // Distance to the original clipped tile box, unchanged.
                    let d = max(max(origin - c, c - box_last), vec3<i32>(0));
                    near = min(near, u32(max(max(d.x, d.y), d.z)));
                }
            }
        }
    }
    // Eight independent min trees; invalid tiles still reach every barrier.
    tile_near_min[li] = near;
    workgroupBarrier();
    for (var width = 16u; width > 0u; width = width >> 1u) {
        if lane < width {
            tile_near_min[li] = min(tile_near_min[li], tile_near_min[li + width]);
        }
        workgroupBarrier();
    }
    if lane == 0u && valid {
        tile_near[t] = tile_near_min[li];
    }
}

// One thread per tile, `tile_near` to the current half of `tile_rank`: the
// rank of the Chebyshev tile distance to the nearest occupied tile (near 0)
// within ring_max, else of ring_max + 1. With `all_tiles`, 0 everywhere.
@compute @workgroup_size(256)
fn tiles_rings(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let t = gid.x;
    let total = ts_tile_count();
    if t >= total {
        return;
    }
    let at = tile_counts[tile_parity_word()] * total + t;
    if u.all_tiles != 0u {
        tile_rank[at] = 0u;
        return;
    }
    let dims = ts_tile_dims();
    let p = ts_grid_coord(t, dims);
    let r = i32(u.ring_max);
    let first = max(p - vec3<i32>(r), vec3<i32>(0));
    let last = min(p + vec3<i32>(r), dims - vec3<i32>(1));
    var ring = u.ring_max + 1u;
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let q = vec3<i32>(x, y, z);
                if tile_near[ts_grid_index(q, dims)] != 0u {
                    continue;
                }
                let d = abs(q - p);
                ring = min(ring, u32(max(max(d.x, d.y), d.z)));
            }
        }
    }
    tile_rank[at] = rank_of(ring, tile_near[t]);
}

// The list rank of a tile: 0 in the cell set C, 1 for the rest of rings 0
// and 1, else its ring.
fn rank_of(ring: u32, near: u32) -> u32 {
    if ring <= 1u {
        return select(1u, 0u, near <= CELL_REACH);
    }
    return ring;
}

// One thread: the lists. A counting sort of the tiles by rank, stable in
// tile order: rank 0 is the cell set C (ring <= 1 and near <= CELL_REACH),
// rank 1 the rest of rings 0 and 1, rank k >= 2 ring k. The ranks' start
// offsets become their ends as the scatter advances them, and the ends are
// the counts: word 0 is |C|, word k >= 1 the tiles with ring <= k. Then the
// triples, the retired list against the previous half (exactly the tiles in
// C then and not now), and the active-fraction stats word (solver word 6,
// `capped` bound at the solver words): |C| / T³.
@compute @workgroup_size(1)
fn tiles_lists() {
    let total = ts_tile_count();
    let r = u.ring_max;
    // An inactive clock slot keeps the last step's table (the parity was not
    // flipped) and switches every list triple off, including retired and
    // full-face dispatches. Active full-face passes retain the dense grid.
    let m = u.n + vec3<u32>(1u);
    tile_args[3u * (r + 2u)] = select(0u, (m.x * m.y * m.z + 255u) / 256u, clock_active());
    tile_args[3u * (r + 2u) + 1u] = 1u;
    tile_args[3u * (r + 2u) + 2u] = 1u;
    let end_triples = 3u * (r + 3u);
    let range_word = (end_triples + 1u) & ~1u;
    if range_word != end_triples { tile_args[end_triples] = 0u; }
    tile_args[range_word] = 0u;
    tile_args[range_word + 1u] = select(0u, u.extension_layers, clock_active());
    if !clock_active() {
        for (var k = 0u; k <= r + 1u; k = k + 1u) {
            tile_args[3u * k] = 0u;
        }
        return;
    }
    let parity = tile_counts[tile_parity_word()];
    let cur = parity * total;
    let prev = (1u - parity) * total;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        tile_counts[k] = 0u;
    }
    for (var t = 0u; t < total; t = t + 1u) {
        let rank = tile_rank[cur + t];
        tile_counts[rank] = tile_counts[rank] + 1u;
    }
    var start = 0u;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        let count = tile_counts[k];
        tile_counts[k] = start;
        start = start + count;
    }
    var retired = 0u;
    for (var t = 0u; t < total; t = t + 1u) {
        let rank = tile_rank[cur + t];
        tiles_by_ring[tile_counts[rank]] = t;
        tile_counts[rank] = tile_counts[rank] + 1u;
        if tile_rank[prev + t] == 0u && rank != 0u {
            tiles_retired[retired] = t;
            retired = retired + 1u;
        }
    }
    tile_counts[r + 2u] = retired;
    for (var k = 0u; k <= r; k = k + 1u) {
        tile_args[3u * k] = 2u * tile_counts[k];
        tile_args[3u * k + 1u] = 1u;
        tile_args[3u * k + 2u] = 1u;
    }
    tile_args[3u * (r + 1u)] = 2u * retired;
    tile_args[3u * (r + 1u) + 1u] = 1u;
    tile_args[3u * (r + 1u) + 2u] = 1u;
    capped[6u] = bitcast<u32>(f32(tile_counts[0]) / f32(total));
}
