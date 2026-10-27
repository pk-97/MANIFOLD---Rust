// ---- FLIP's own passes over the world tiles (the build itself is
// world_tiles.wgsl): the canonical element, fresh and retired slots, the
// solid corners, the tally, and the test-only poisons. ----

// Thread `gid` of a list dispatched at 512 threads a slot (the triple over
// every held slot, or C's): the pool index of its cell, which it sets as
// its home.
fn list_cell(gid: u32) -> u32 {
    let idx = ts_list_cell(tiles_by_ring[gid >> 9u], gid);
    ts_home(idx);
    return idx;
}

// The solid corner's value wherever no tile is stored: clear of every solid
// by more than any reach the passes read (the move's march, the open
// fractions' corners), since an absent tile is at least a tile from water.
const CANON_CORNER_CELLS: f32 = 3.0;

// The canonical φ in cells: particle_distance's start, kept where no
// particle is within its scan.
const CANON_PHI_CELLS: f32 = 3.0;

// The canonical cell values: water 0, φ CANON_PHI_CELLS, rhs 0.
fn canonical_cell(idx: u32) {
    tile_water[idx] = 0.0;
    tile_phi[idx] = CANON_PHI_CELLS * u.cell_size;
    tile_rhs[idx] = 0.0;
}

// The gather's output at a record with no particle within reach: every face
// absent (velocity 0, weight 0).
const CANONICAL_FACE: FaceSample = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));

// The tile's face grids that are not gathered or solid, and the cell fields
// the retire leaves alone: the values a pass would read before writing.
fn fresh_fields(idx: u32) {
    let none = CANONICAL_FACE;
    faces_out[idx] = none;
    fresh_a[idx] = none;
    fresh_b[idx] = none;
    fresh_f[idx] = none;
    fresh_faces[idx] = none;
    fresh_s[idx] = none;
    fresh_v[idx] = none;
    fresh_pressure[idx] = 0.0;
    fresh_solve_water[idx] = 0.0;
    pocket[idx] = 0u;
    pocket_label[idx] = 0u;
    atomicStore(&pocket_leader[idx], 0u);
}

// One thread, once per pool build: the canonical element of every pooled
// field. It stands for every cell and record of an absent tile, so every
// face is absent there; the corners are clear of the solids.
@compute @workgroup_size(1)
fn pool_canon() {
    let at = ts_canon();
    canonical_cell(at);
    fresh_fields(at);
    tile_corners[at] = CANON_CORNER_CELLS * u.cell_size;
}

// 512 threads a fresh slot (the triple at world_tiles.rs
// `fresh_args_offset`): every pooled field's cells and records of the slot's
// tile to their canonical values, as the tile was never written. An inactive
// clock slot's triple is off.
@compute @workgroup_size(256)
fn tiles_fresh(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = ts_list_cell(tiles_fresh_list[gid.x >> 9u], gid.x);
    ts_home(idx);
    canonical_cell(idx);
    fresh_fields(idx);
}

// 512 threads a retired slot (the triple at `retired_args_offset`): its
// cells and their records back to canonical, so a tile leaving C reads as
// the dense passes would leave it.
@compute @workgroup_size(256)
fn tiles_retire(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = ts_list_cell(tiles_retired[gid.x >> 9u], gid.x);
    ts_home(idx);
    canonical_cell(idx);
    faces_out[idx] = CANONICAL_FACE;
}

// 512 threads a held slot (the triple over every held slot), before any
// pass reads the solids: corner p, at origin + p·h, is the solid distance
// there (`solid_distance`: the Wall faces and this step's posed bodies), for
// every cell of the tile.
@compute @workgroup_size(256)
fn solid_corners(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let x = fma(vec3<f32>(ts_cell_coord(idx)), vec3<f32>(u.cell_size), u.origin);
    tile_corners[idx] = solid_distance(x);
}

// One thread, after the step's first sort (`capped` bound at the solver
// words): the held share of the pool, |C| / P, into solver word 6, and the
// particles whose own tile has no slot (an exhausted pool; the sort keys
// them on the canonical element) added to the tick's removed count. The
// move removes them.
@compute @workgroup_size(1)
fn tiles_tally() {
    if !clock_active() { return; }
    capped[6u] = bitcast<u32>(f32(tile_counts[0]) / f32(ts_pool_tiles()));
    capped[TILE_REMOVED_TAIL] += ranges[ts_canon()].count;
}

// Test-only (gpu_flip_tile_tests.rs): 512 threads a held slot, after
// tiles_retire; NaN into the water, φ and rhs of every cell of a slot with
// rank >= 2 (past rings 0 and 1), so a read that escapes the defined-value
// rule shows in the particles or the faces.
@compute @workgroup_size(256)
fn poison_inactive(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >> 9u) < tile_counts[1] {
        return;
    }
    let idx = list_cell(gid.x);
    let nan = bitcast<f32>(0x7fc00000u);
    tile_water[idx] = nan;
    tile_phi[idx] = nan;
    tile_rhs[idx] = nan;
}

// Test-only (gpu_flip_tile_tests.rs): one thread per pool element, after
// tiles_fresh; NaN into every pooled field's cell and record of each free
// slot, so a read through a stale or absent slot shows in the particles or
// the faces.
@compute @workgroup_size(256)
fn poison_free(@builtin(global_invocation_id) gid: vec3<u32>) {
    let slot = gid.x >> 9u;
    if slot >= ts_pool_tiles() || ts_slot_entry(slot) != WT_NONE {
        return;
    }
    let idx = slot * 512u + (gid.x & 511u);
    let nan = bitcast<f32>(0x7fc00000u);
    let bad = FaceSample(vec4<f32>(nan), vec4<f32>(nan));
    tile_water[idx] = nan;
    tile_phi[idx] = nan;
    tile_rhs[idx] = nan;
    tile_corners[idx] = nan;
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
