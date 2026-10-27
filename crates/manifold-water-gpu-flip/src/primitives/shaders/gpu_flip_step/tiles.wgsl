// ---- The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
// table)) and the tile pool's allocator (WATER_TILE_POOL_DESIGN.md section
// 4). Built every step from the particles, on the GPU, never read
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

fn tile_exhausted_word() -> u32 {
    return u.ring_max + 5u;
}

fn tile_fresh_word() -> u32 {
    return u.ring_max + 6u;
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
    let end_triples = 3u * (u.ring_max + 2u);
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

// One thread, after `tiles_rings`: the whole allocation, in tile order, so
// slot numbers never depend on thread timing. (1) Every held tile whose ring
// is past `alloc_ring` pushes its slot onto the free stack and leaves the
// table. (2) Every absent tile with ring <= 1 (the cell set C lies in it) pops
// the stack's top slot. (3) Then every absent tile with ring in 2..=alloc_ring
// does. A tile finding the stack empty stays absent and sets the exhausted
// word (rewritten every active step). Each newly held tile joins the fresh
// list, whose indirect triple is written last (512 threads a tile, 256 a
// group). An inactive clock slot allocates nothing: its fresh triple is
// switched off and the table stays.
@compute @workgroup_size(1)
fn tiles_allocate() {
    let word = fresh_args_word();
    tile_args[word + 1u] = 1u;
    tile_args[word + 2u] = 1u;
    if !clock_active() {
        tile_args[word] = 0u;
        return;
    }
    let total = ts_tile_count();
    var top = counts_load(tile_free_top_word());
    var fresh = 0u;
    var exhausted = 0u;
    for (var t = 0u; t < total; t = t + 1u) {
        let slot = tile_table[t];
        if slot != TS_NONE && tile_ring(t) > u.alloc_ring {
            tile_free[top] = slot;
            top = top + 1u;
            tile_table[t] = TS_NONE;
            tile_table[total + slot] = TS_NONE;
        }
    }
    for (var round = 0u; round < 2u; round = round + 1u) {
        for (var t = 0u; t < total; t = t + 1u) {
            if tile_table[t] != TS_NONE {
                continue;
            }
            let ring = tile_ring(t);
            let wanted = select(ring >= 2u && ring <= u.alloc_ring, ring <= 1u, round == 0u);
            if !wanted {
                continue;
            }
            if top == 0u {
                exhausted = 1u;
                continue;
            }
            top = top - 1u;
            let slot = tile_free[top];
            tile_table[t] = slot;
            tile_table[total + slot] = t;
            tiles_fresh_list[fresh] = t;
            fresh = fresh + 1u;
        }
    }
    counts_store(tile_free_top_word(), top);
    counts_store(tile_exhausted_word(), exhausted);
    counts_store(tile_fresh_word(), fresh);
    tile_args[word] = 2u * fresh;
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

// Test-only (gpu_flip_tile_tests.rs): one thread per pool element, after
// tiles_fresh; NaN into every pooled field's cell and record of each slot on
// the free stack, so a read through a stale or absent slot shows in the
// particles or the faces.
@compute @workgroup_size(256)
fn poison_free(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x >> 9u;
    if i >= counts_load(tile_free_top_word()) {
        return;
    }
    let idx = tile_free[i] * 512u + (gid.x & 511u);
    let nan = bitcast<f32>(0x7fc00000u);
    let bad = FaceSample(vec4<f32>(nan), vec4<f32>(nan));
    tile_water[idx] = nan;
    tile_phi[idx] = nan;
    tile_rhs[idx] = nan;
    faces_out[idx] = bad;
    fresh_a[idx] = bad;
    fresh_b[idx] = bad;
    fresh_f[idx] = bad;
    fresh_s[idx] = bad;
    fresh_v[idx] = bad;
    fresh_faces[idx] = bad;
    fresh_pressure[idx] = nan;
    fresh_solve_water[idx] = nan;
    pocket[idx] = 0x7fc00000u;
    pocket_label[idx] = 0x7fc00000u;
    atomicStore(&pocket_leader[idx], 0x7fc00000u);
}

// One thread per tile: `tile_near` to its start value, CELL_REACH + 1 (no
// particle within reach), or 0 under `all_tiles`. Thread 0 flips the ring
// halves' parity for this step first: no other thread of this pass reads
// it, and `tiles_rings` runs after. An inactive clock slot runs none of the
// table's passes, so the next active step's retire measures against the last
// active step's C.
@compute @workgroup_size(256)
fn tiles_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    if gid.x == 0u {
        counts_store(tile_parity_word(), 1u - counts_load(tile_parity_word()));
    }
    if gid.x < ts_tile_count() {
        atomicStore(&tile_near[gid.x], select(CELL_REACH + 1u, 0u, u.all_tiles != 0u));
    }
}

// One thread per slot in use (`slot_record`), after `tiles_clear`: a live
// particle (radius positive, as the step's sort counts it) in cell c, binned as the sort bins
// it (the same clamp), lowers `tile_near` of every tile within CELL_REACH
// cells to the Chebyshev cell distance from c to the tile's box clipped to
// the lattice. So a tile's value is the distance to the nearest
// particle-holding cell within reach, CELL_REACH + 1 when none. Under
// `all_tiles` every tile is already 0.
@compute @workgroup_size(256)
fn tiles_mark(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() || u.all_tiles != 0u { return; }
    let i = gid.x;
    if i >= slots_in_use() { return; }
    let particle = sorted[i];
    if !(particle.position_radius.w > 0.0) { return; }
    let n = ts_cells();
    let c = clamp(
        vec3<i32>(floor((particle.position_radius.xyz - u.box_min) * u.inv_cell)),
        vec3<i32>(0),
        n - vec3<i32>(1),
    );
    let reach = vec3<i32>(i32(CELL_REACH));
    let dims = ts_tile_dims();
    let first = max(c - reach, vec3<i32>(0)) >> vec3<u32>(3u);
    let last = min(c + reach, n - vec3<i32>(1)) >> vec3<u32>(3u);
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let q = vec3<i32>(x, y, z);
                let origin = q * i32(TS_TILE);
                let box_last = min(origin + vec3<i32>(i32(TS_TILE) - 1), n - vec3<i32>(1));
                let d = max(max(origin - c, c - box_last), vec3<i32>(0));
                atomicMin(&tile_near[ts_grid_index(q, dims)], u32(max(max(d.x, d.y), d.z)));
            }
        }
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
                if atomicLoad(&tile_near[ts_grid_index(q, dims)]) != 0u {
                    continue;
                }
                let d = abs(q - p);
                ring = min(ring, u32(max(max(d.x, d.y), d.z)));
            }
        }
    }
    tile_rank[at] = rank_of(ring, atomicLoad(&tile_near[t]));
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
    // flipped) and switches every list triple off, the retired list's too.
    let end_triples = 3u * (r + 2u);
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
