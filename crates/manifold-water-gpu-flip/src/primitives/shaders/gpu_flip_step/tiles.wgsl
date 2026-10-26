// ---- The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
// table)) and the tile pool's allocator (WATER_TILE_POOL_DESIGN.md section
// 4). Built every step from the sort's ranges, on the GPU, never read
// back; the parity and the lists live here so a replayed encode stays
// right. ----

// The cell passes' reach from a particle-holding cell.
const CELL_REACH: u32 = 2u;

fn tile_parity_word() -> u32 {
    return u.ring_max + 3u;
}

// The allocator's words follow the parity word.
fn tile_free_top_word() -> u32 {
    return u.ring_max + 4u;
}

fn tile_pop_word() -> u32 {
    return u.ring_max + 5u;
}

fn tile_exhausted_word() -> u32 {
    return u.ring_max + 6u;
}

fn tile_fresh_word() -> u32 {
    return u.ring_max + 7u;
}

fn counts_load(i: u32) -> u32 {
    return atomicLoad(&tile_counts[i]);
}

fn counts_store(i: u32, value: u32) {
    atomicStore(&tile_counts[i], value);
}

// The word of `tile_args` holding the fresh list's triple, after the
// aligned replay range.
fn fresh_args_word() -> u32 {
    let end_triples = 3u * (u.ring_max + 3u);
    return ((end_triples + 1u) & ~1u) + 2u;
}

// The cell passes run over the cell set C, the first `tile_counts[0]` tiles
// of `tiles_by_ring`, through the triple at `tile_args[0]`. The pool index
// of the thread's cell; a cell's records live at the same index.
fn c_cell_index(gid: u32) -> u32 {
    return ts_list_cell(tiles_by_ring[gid >> 9u], gid);
}

// The gather's output at a record with no particle within reach: every face
// absent (velocity 0, weight 0) except a box wall face, closed (0, weight 1).
// `mask` is `ts_wall_mask` of the record.
fn canonical_face(mask: u32) -> FaceSample {
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    for (var a = 0u; a < 3u; a = a + 1u) {
        if ((mask >> a) & 1u) != 0u {
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

// The tile's face grids that are not gathered or solid, and the cell fields
// the retire leaves alone: the values a pass would read before writing.
fn fresh_fields(idx: u32, face: FaceSample) {
    let none = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    faces_out[idx] = face;
    fresh_a[idx] = face;
    fresh_b[idx] = face;
    fresh_f[idx] = face;
    fresh_faces[idx] = face;
    fresh_s[idx] = none;
    fresh_v[idx] = none;
    fresh_pressure[idx] = 0.0;
    fresh_solve_water[idx] = 0.0;
    pocket[idx] = 0u;
    pocket_label[idx] = 0u;
    atomicStore(&pocket_leader[idx], 0u);
}

// One thread per canonical element (8), once per pool build: the canonical
// block of every pooled field. Cell fields hold their defined value in all
// eight (only the first is ever read); a record field holds `canonical_face`
// of its wall mask, except the open fractions `s` and the solid velocity
// `v`, which are zero for every mask: the record past the lattice is what
// `open_fractions` and `solid_face_velocity` write there (no face of it is
// an inner face, and it owns no cell).
@compute @workgroup_size(8)
fn pool_canon(@builtin(global_invocation_id) gid: vec3<u32>) {
    let mask = gid.x;
    if mask >= 8u {
        return;
    }
    let at = ts_canon(mask);
    let face = canonical_face(mask);
    canonical_cell(at);
    fresh_fields(at, face);
}

// One thread per box tile, after the rings: a stored tile whose ring is
// past `alloc_ring` returns its slot to the free stack and leaves the table.
// Thread 0 clears the step's exhausted flag and fresh count first; the
// allocation runs in the next dispatch.
@compute @workgroup_size(256)
fn tiles_free(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if gid.x == 0u {
        counts_store(tile_exhausted_word(), 0u);
        counts_store(tile_fresh_word(), 0u);
    }
    let t = gid.x;
    let total = ts_tile_count();
    if t >= total {
        return;
    }
    let slot = tile_table[t];
    if slot == TS_NONE || tile_ring(t) <= u.alloc_ring {
        return;
    }
    let at = atomicAdd(&tile_counts[tile_free_top_word()], 1u);
    tile_free[at] = slot;
    tile_table[t] = TS_NONE;
    tile_table[total + slot] = TS_NONE;
}

// One thread per box tile, after `tiles_free`: an absent tile with ring at
// most `alloc_ring` takes the free stack's k-th slot from the top (k the
// order its thread asks in). Slot numbers follow that order; no value
// depends on them. A tile finding the stack empty stays absent and sets the
// exhausted flag. A taken tile joins the fresh list.
@compute @workgroup_size(256)
fn tiles_alloc(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let t = gid.x;
    let total = ts_tile_count();
    if t >= total || tile_table[t] != TS_NONE || tile_ring(t) > u.alloc_ring {
        return;
    }
    let top = counts_load(tile_free_top_word());
    let k = atomicAdd(&tile_counts[tile_pop_word()], 1u);
    if k >= top {
        counts_store(tile_exhausted_word(), 1u);
        return;
    }
    let slot = tile_free[top - 1u - k];
    tile_table[t] = slot;
    tile_table[total + slot] = t;
    let at = atomicAdd(&tile_counts[tile_fresh_word()], 1u);
    tiles_fresh_list[at] = t;
}

// One thread, after `tiles_alloc`: the stack loses the slots taken, the
// pop count restarts, and the fresh list's triple is written (512 threads
// a tile, 256 a group). An inactive clock slot allocates nothing: its
// fresh triple is switched off.
@compute @workgroup_size(1)
fn tiles_settle() {
    let word = fresh_args_word();
    tile_args[word + 1u] = 1u;
    tile_args[word + 2u] = 1u;
    if !clock_active() {
        tile_args[word] = 0u;
        return;
    }
    let top = counts_load(tile_free_top_word());
    let taken = min(counts_load(tile_pop_word()), top);
    counts_store(tile_free_top_word(), top - taken);
    counts_store(tile_pop_word(), 0u);
    tile_args[word] = 2u * counts_load(tile_fresh_word());
}

// 512 threads a fresh tile: every pooled field's cells and records of the
// tile to their canonical values, as the tile was never written.
@compute @workgroup_size(256)
fn tiles_fresh(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = ts_list_cell(tiles_fresh_list[gid.x >> 9u], gid.x);
    if idx == TS_NONE {
        return;
    }
    canonical_cell(idx);
    fresh_fields(idx, canonical_face(ts_wall_mask(ts_cell_coord(idx))));
}

// 512 threads a retired tile (the triple at `tile_args[3 · (ring_max + 1)]`):
// its cells and their records back to canonical, so a tile leaving C reads
// as the dense passes would leave it. Wall records past the lattice are
// never written by the C passes and stay canonical.
@compute @workgroup_size(256)
fn tiles_retire(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = ts_list_cell(tiles_retired[gid.x >> 9u], gid.x);
    if idx == TS_NONE {
        return;
    }
    canonical_cell(idx);
    faces_out[idx] = canonical_face(ts_wall_mask(ts_cell_coord(idx)));
}

// The ring of a tile in the current half of the rank: rings 0 and 1 share
// rank 0 and 1, so both read as 1.
fn tile_ring(tile: u32) -> u32 {
    let rank = tile_rank[counts_load(tile_parity_word()) * ts_tile_count() + tile];
    return select(rank, 1u, rank <= 1u);
}

// Test-only (gpu_flip_tile_tests.rs): one thread per cell, after
// tiles_retire; NaN into the water, φ and rhs of every stored cell of a tile
// with ring > 1 (rank >= 2), so a read that escapes the defined-value rule
// shows in the particles or the faces.
@compute @workgroup_size(256)
fn poison_inactive(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= ts_cell_count() {
        return;
    }
    let p = ts_box_cell(idx);
    let half = counts_load(tile_parity_word()) * ts_tile_count();
    if tile_rank[half + ts_tile_of_cell(p)] < 2u || !ts_cell_stored(p) {
        return;
    }
    let at = ts_cell_at(p);
    let nan = bitcast<f32>(0x7fc00000u);
    tile_water[at] = nan;
    tile_phi[at] = nan;
    tile_rhs[at] = nan;
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
        counts_store(tile_parity_word(), 1u - counts_load(tile_parity_word()));
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
                if ranges[ts_dense_cell(c)].count != 0u {
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
    let at = counts_load(tile_parity_word()) * total + t;
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
    let parity = counts_load(tile_parity_word());
    let cur = parity * total;
    let prev = (1u - parity) * total;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        counts_store(k, 0u);
    }
    for (var t = 0u; t < total; t = t + 1u) {
        let rank = tile_rank[cur + t];
        counts_store(rank, counts_load(rank) + 1u);
    }
    var start = 0u;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        let count = counts_load(k);
        counts_store(k, start);
        start = start + count;
    }
    var retired = 0u;
    for (var t = 0u; t < total; t = t + 1u) {
        let rank = tile_rank[cur + t];
        tiles_by_ring[counts_load(rank)] = t;
        counts_store(rank, counts_load(rank) + 1u);
        if tile_rank[prev + t] == 0u && rank != 0u {
            tiles_retired[retired] = t;
            retired = retired + 1u;
        }
    }
    counts_store(r + 2u, retired);
    for (var k = 0u; k <= r; k = k + 1u) {
        tile_args[3u * k] = 2u * counts_load(k);
        tile_args[3u * k + 1u] = 1u;
        tile_args[3u * k + 2u] = 1u;
    }
    tile_args[3u * (r + 1u)] = 2u * retired;
    tile_args[3u * (r + 1u) + 1u] = 1u;
    tile_args[3u * (r + 1u) + 2u] = 1u;
    capped[6u] = bitcast<u32>(f32(counts_load(0u)) / f32(total));
}

// One thread per record of the box, at the end of an active slot: the
// pooled faces and φ copied to the step's dense outputs. A record past the
// lattice reads its canonical record, so the output keeps its walls.
@compute @workgroup_size(256)
fn publish(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = gid.x;
    if idx >= ts_record_count() {
        return;
    }
    let p = ts_box_record(idx);
    faces_published[ts_corner(p)] = faces_in[ts_face(p)];
    if all(p < ts_cells()) {
        distance_published[ts_dense_cell(p)] = phi[ts_cell_at(p)];
    }
}
