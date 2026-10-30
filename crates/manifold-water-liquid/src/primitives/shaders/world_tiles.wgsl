// World tiles, the build: the
// table every pooled lattice field is read through, rebuilt on the GPU every
// active step from a set of points and never read back. Composed after the
// read side (`manifold_node_engine::scene::world_tiles::READ_SIDE`) and a
// points snippet (`world_tiles_liquid.wgsl`, `world_tiles_parents.wgsl`),
// which binds its source at binding 13 and defines
// `fn wt_point_live(i: u32) -> bool` (false past the build's points) and
// `fn wt_point_cell(i: u32) -> vec3<i32>`, the world cell of point i.
//
// Passes, a barrier between each (world_tiles.rs `encode_build`): wt_clear,
// wt_seed, wt_seeds, wt_dilate, wt_rings, wt_near, wt_carry, wt_wants, the
// prefix scan, wt_free, wt_allocate, wt_slots, then wt_finish. Inserts and
// lookups never share a pass, so a lookup never meets a key in motion. An
// insert can move a key to the next entry, so an entry's own words are
// written only after the dilation's inserts: the seed pass, which runs
// before them, keeps its tiles in words of its own.
//
// Determinism: ordered linear probing makes the table's layout a function of
// its key set alone, whatever order threads insert in; slots are freed in
// slot order and handed out in table order (the cell set C first) by a prefix
// scan, so slot numbers are the same on every run. The lists are filled by
// atomic append and their order is free: readers index partials by slot.
//
// An inactive clock slot (the gate, a GPU FLIP clock plan: live with no time
// to step) runs none of it and keeps the table; wt_finish switches the list
// triples off for it.

struct WtParams {
    // The lattice origin (−h/2 on every axis) and 1 / h, for the points'
    // cells.
    origin: vec3<f32>,
    inv_cell: f32,
    // P and H.
    pool_tiles: u32,
    entries: u32,
    // Tiles within ring_max of a point's tile join the table; rings past
    // alloc_ring get no slot.
    ring_max: u32,
    alloc_ring: u32,
    // The cell set C's reach from a point's cell, in cells.
    cell_reach: u32,
    // The level below's slots, the parents build's points.
    point_count: u32,
    // 1: every slot is freed and handed out again in table order (the
    // coarse levels' parents build).
    from_scratch: u32,
    // Written into the args' two replay ranges on an active step.
    replay_first: u32,
    replay_second: u32,
    // The level below's H and P (parents build only).
    child_entries: u32,
    child_pool_tiles: u32,
    // The tiles a ring may hold, first and last per axis (the build's clip).
    clip_lo_x: i32,
    clip_lo_y: i32,
    clip_lo_z: i32,
    clip_hi_x: i32,
    clip_hi_y: i32,
    clip_hi_z: i32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> wt: WtParams;
// The table (`world_tiles_read.wgsl` has its layout).
@group(0) @binding(1) var<storage, read_write> wt_table: array<atomic<u32>>;
// Word k < ring_max + 2: held slots with rank <= k (word 0 is |C|); then the
// retired count, the free stack's top, the exhausted bits and the fresh
// count (world_tiles.rs `retired_word` and on).
@group(0) @binding(2) var<storage, read_write> wt_counts: array<atomic<u32>>;
// Indirect triples: list k (rank <= k) at word 3k for k <= ring_max, the
// retired list at 3(ring_max + 1), the first replay range at the next even
// word, the fresh list after it, then the second replay range.
@group(0) @binding(3) var<storage, read_write> wt_args: array<u32>;
// Held slots by rank: rank 0 (C) first, then rank by rank.
@group(0) @binding(4) var<storage, read_write> wt_by_rank: array<u32>;
// Slots carried from C last build into a rank past 0.
@group(0) @binding(5) var<storage, read_write> wt_retired: array<u32>;
// Slots handed out this build.
@group(0) @binding(6) var<storage, read_write> wt_fresh: array<u32>;
// Per entry: ring [0, H), near [H, 2H), tile x, y, z [2H, 5H) as bits
// (WT_UNSET until written); the seed pass's tile x, y, z [5H, 8H), at the
// entry its lookup found; the seed list [8H, 9H), each seed's entry there.
@group(0) @binding(7) var<storage, read_write> wt_entry: array<atomic<u32>>;
// Per slot: its state this build [0, P), its rank [P, 2P) (last build's
// until wt_slots), the free stack [2P, 3P).
@group(0) @binding(8) var<storage, read_write> wt_slot: array<u32>;
// The rank histogram, the list cursors per rank, the keys held, the seed
// count, the retired and fresh cursors (world_tiles.rs `build_words`).
@group(0) @binding(9) var<storage, read_write> wt_build: array<atomic<u32>>;
// The dilation's indirect triple, raised by wt_seeds.
@group(0) @binding(10) var<storage, read_write> wt_build_args: array<atomic<u32>>;
// Level 0 of the prefix scan: freed flags per slot [0, P), then the wants of
// the cell set C (rank 0) per entry [P, P + H), then of the rest up to
// alloc_ring [P + H, P + 2H).
@group(0) @binding(11) var<storage, read_write> wt_scan: array<u32>;
// A GPU FLIP clock plan (gpu_flip_clock.wgsl Plan; word 0 step_dt, word 11
// live_mode). Zeros run.
@group(0) @binding(12) var<storage, read> wt_gate: array<u32>;
// The other material's table this build shares its key set with (bound only
// for wt_foreign, wt_share and wt_repoint): the same layout and H, its own
// pool of shared_pool_tiles slots.
@group(0) @binding(16) var<storage, read_write> wt_shared: array<atomic<u32>>;

fn ts_pool_tiles() -> u32 {
    return wt.pool_tiles;
}

fn ts_table_word(i: u32) -> u32 {
    return atomicLoad(&wt_table[i]);
}

// Exhausted bits (world_tiles.rs POOL_SHORT, TABLE_FULL, ALIAS).
const WT_POOL_SHORT: u32 = 1u;
const WT_TABLE_FULL: u32 = 2u;
const WT_ALIAS: u32 = 4u;
// An entry's tile axis before any writer: i32::MIN, no tile's coordinate.
const WT_UNSET: u32 = 0x80000000u;
// Slot states.
const WT_FREE: u32 = 0u;
const WT_CARRIED: u32 = 1u;
const WT_FRESH: u32 = 2u;

fn wt_off() -> bool {
    return wt_gate[11] != 0u && !(bitcast<f32>(wt_gate[0]) > 0.0);
}

// Ranks run 0..=ring_max + 1.
fn wt_ranks() -> u32 {
    return wt.ring_max + 2u;
}

fn wt_retired_word() -> u32 {
    return wt.ring_max + 2u;
}

fn wt_free_top_word() -> u32 {
    return wt.ring_max + 3u;
}

fn wt_exhausted_word() -> u32 {
    return wt.ring_max + 4u;
}

fn wt_fresh_word() -> u32 {
    return wt.ring_max + 5u;
}

fn wt_hist_word(rank: u32) -> u32 {
    return rank;
}

fn wt_cursor_word(rank: u32) -> u32 {
    return wt_ranks() + rank;
}

fn wt_keys_word() -> u32 {
    return 2u * wt_ranks();
}

fn wt_seeds_word() -> u32 {
    return 2u * wt_ranks() + 1u;
}

fn wt_retired_cursor() -> u32 {
    return 2u * wt_ranks() + 2u;
}

fn wt_fresh_cursor() -> u32 {
    return 2u * wt_ranks() + 3u;
}

fn wt_ring_word(e: u32) -> u32 {
    return e;
}

fn wt_near_word(e: u32) -> u32 {
    return wt.entries + e;
}

fn wt_coord_word(e: u32, axis: u32) -> u32 {
    return (2u + axis) * wt.entries + e;
}

fn wt_seed_coord_word(e: u32, axis: u32) -> u32 {
    return (5u + axis) * wt.entries + e;
}

fn wt_seed_word(j: u32) -> u32 {
    return 8u * wt.entries + j;
}

fn wt_state_word(s: u32) -> u32 {
    return s;
}

fn wt_rank_word(s: u32) -> u32 {
    return wt.pool_tiles + s;
}

fn wt_stack_word(i: u32) -> u32 {
    return 2u * wt.pool_tiles + i;
}

// Word of the table holding entry e's key.
fn wt_key_word(e: u32) -> u32 {
    return WT_HEADER + e;
}

// Word of the table holding entry e's slot.
fn wt_entry_slot_word(e: u32) -> u32 {
    return WT_HEADER + wt.entries + e;
}

fn wt_slot_row(s: u32) -> u32 {
    return WT_HEADER + 2u * wt.entries + WT_SLOT_WORDS * s;
}

// The replay range's word in the args: the even word after the triples.
fn wt_range_word() -> u32 {
    return (3u * (wt.ring_max + 2u) + 1u) & ~1u;
}

fn wt_flag(bit: u32) {
    atomicOr(&wt_counts[wt_exhausted_word()], bit);
}

// The list rank: 0 in the cell set C (ring <= 1 and a point's cell within
// cell_reach), 1 for the rest of rings 0 and 1, else the ring.
fn wt_rank(ring: u32, near: u32) -> u32 {
    if ring <= 1u {
        return select(1u, 0u, near <= wt.cell_reach);
    }
    return ring;
}

fn wt_entry_rank(e: u32) -> u32 {
    return wt_rank(atomicLoad(&wt_entry[wt_ring_word(e)]), atomicLoad(&wt_entry[wt_near_word(e)]));
}

fn wt_entry_tile(e: u32) -> vec3<i32> {
    return bitcast<vec3<i32>>(vec3<u32>(
        atomicLoad(&wt_entry[wt_coord_word(e, 0u)]),
        atomicLoad(&wt_entry[wt_coord_word(e, 1u)]),
        atomicLoad(&wt_entry[wt_coord_word(e, 2u)]),
    ));
}

// Ordered insert (Shun and Blelloch 2014, phase-concurrent linear probing):
// walk from the key's hash; an empty word or a smaller key is swapped for the
// key in hand and a displaced key walks on from the next word; an equal key
// stops the walk. Whatever order threads arrive in, the words left are the
// ordered table of the key set. A walk that covers the whole table found it
// full: the key in hand is dropped and the table-full bit set, so the GPU
// never hangs.
fn wt_insert(key: u32) {
    let mask = wt.entries - 1u;
    var k = key;
    var i = ts_hash(k, wt.entries);
    var walked = 0u;
    loop {
        if walked >= wt.entries {
            wt_flag(WT_TABLE_FULL);
            return;
        }
        let c = atomicLoad(&wt_table[wt_key_word(i)]);
        if c == k {
            return;
        }
        if c < k {
            let swap = atomicCompareExchangeWeak(&wt_table[wt_key_word(i)], c, k);
            if !swap.exchanged {
                // Lost to another writer (or a spurious failure): read the
                // word again.
                continue;
            }
            if c == WT_EMPTY {
                atomicAdd(&wt_build[wt_keys_word()], 1u);
                return;
            }
            k = c;
        }
        i = (i + 1u) & mask;
        walked = walked + 1u;
    }
}

// Write tile t into entry e's words starting at `region` · H (2 the
// entry's tile, 5 the seed pass's), each axis by compare-exchange from
// WT_UNSET. Every writer of an entry writes the same tile unless two live
// tiles share its key (an alias): a writer finding another value sets the
// alias bit. True for the one writer of the x axis.
fn wt_claim_in(region: u32, e: u32, t: vec3<i32>) -> bool {
    let bits = bitcast<vec3<u32>>(t);
    var first = false;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let at = (region + axis) * wt.entries + e;
        var old = WT_UNSET;
        loop {
            let swap = atomicCompareExchangeWeak(&wt_entry[at], WT_UNSET, bits[axis]);
            old = swap.old_value;
            if swap.exchanged || old != WT_UNSET {
                break;
            }
        }
        if old == WT_UNSET {
            first = first || axis == 0u;
        } else if old != bits[axis] {
            wt_flag(WT_ALIAS);
        }
    }
    return first;
}

fn wt_claim(e: u32, t: vec3<i32>) -> bool {
    return wt_claim_in(2u, e, t);
}

// The tile seed j dilates around.
fn wt_seed_tile(j: u32) -> vec3<i32> {
    let e = atomicLoad(&wt_entry[wt_seed_word(j)]);
    return bitcast<vec3<i32>>(vec3<u32>(
        atomicLoad(&wt_entry[wt_seed_coord_word(e, 0u)]),
        atomicLoad(&wt_entry[wt_seed_coord_word(e, 1u)]),
        atomicLoad(&wt_entry[wt_seed_coord_word(e, 2u)]),
    ));
}

// Tiles per side of the dilation cube.
fn wt_side() -> u32 {
    return 2u * wt.ring_max + 1u;
}

// Offset `o` of the dilation cube, each axis -ring_max..=ring_max.
fn wt_offset(o: u32) -> vec3<i32> {
    let side = wt_side();
    let cell = vec3<u32>(o % side, (o / side) % side, o / (side * side));
    return vec3<i32>(cell) - vec3<i32>(i32(wt.ring_max));
}

// Whether tile t is inside the build's clip.
fn wt_in_clip(t: vec3<i32>) -> bool {
    return all(t >= vec3<i32>(wt.clip_lo_x, wt.clip_lo_y, wt.clip_lo_z))
        && all(t <= vec3<i32>(wt.clip_hi_x, wt.clip_hi_y, wt.clip_hi_z));
}

// The point's tile.
fn wt_point_tile(i: u32) -> vec3<i32> {
    return wt_point_cell(i) >> vec3<u32>(3u);
}

fn wt_live(i: u32) -> bool {
    return wt_point_live(i);
}

// H threads: every key empty, every entry without a slot, ring and near past
// reach, no tile; thread 0 zeroes the build words and the exhausted bits and
// sets the dilation's triple to no groups.
@compute @workgroup_size(256)
fn wt_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if wt_off() {
        return;
    }
    let e = gid.x;
    if e == 0u {
        for (var w = 0u; w < 2u * wt_ranks() + 4u; w = w + 1u) {
            atomicStore(&wt_build[w], 0u);
        }
        atomicStore(&wt_counts[wt_exhausted_word()], 0u);
        atomicStore(&wt_build_args[0], 0u);
        atomicStore(&wt_build_args[1], 1u);
        atomicStore(&wt_build_args[2], 1u);
    }
    if e >= wt.entries {
        return;
    }
    atomicStore(&wt_table[wt_key_word(e)], WT_EMPTY);
    atomicStore(&wt_table[wt_entry_slot_word(e)], WT_NONE);
    atomicStore(&wt_entry[wt_ring_word(e)], wt.ring_max + 1u);
    atomicStore(&wt_entry[wt_near_word(e)], wt.cell_reach + 1u);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        atomicStore(&wt_entry[wt_coord_word(e, axis)], WT_UNSET);
        atomicStore(&wt_entry[wt_seed_coord_word(e, axis)], WT_UNSET);
    }
}

// One thread a point: insert the point's own tile. No guard: the water's
// own tiles always go in first.
@compute @workgroup_size(256)
fn wt_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if wt_off() || !wt_live(gid.x) {
        return;
    }
    wt_insert(ts_tile_key(wt_point_tile(gid.x)));
}

// One thread a point: write the point's tile into its entry; the first
// writer appends the entry to the seed list (order free: each seed's work is
// order-independent) and raises the dilation's triple to cover the list. A
// tile a full table dropped is no seed. With a dilation to follow, the tile
// goes in the seed pass's words and wt_rings gives it ring 0 at the entry
// it ends in; with none (ring_max 0) no key moves again, so the entry's own
// words and ring 0 are written here.
@compute @workgroup_size(256)
fn wt_seeds(@builtin(global_invocation_id) gid: vec3<u32>) {
    if wt_off() || !wt_live(gid.x) {
        return;
    }
    let t = wt_point_tile(gid.x);
    let e = ts_entry_of(ts_tile_key(t));
    let final_entry = wt.ring_max == 0u;
    if e == WT_NONE || !wt_claim_in(select(5u, 2u, final_entry), e, t) {
        return;
    }
    if final_entry {
        atomicMin(&wt_entry[wt_ring_word(e)], 0u);
    }
    let j = atomicAdd(&wt_build[wt_seeds_word()], 1u);
    atomicStore(&wt_entry[wt_seed_word(j)], e);
    let side = wt_side();
    atomicMax(&wt_build_args[0], ((j + 1u) * side * side * side + 255u) / 256u);
}

// (2·ring_max + 1)³ threads a seed (the dilation's triple): insert every tile
// within ring_max of the seed and inside the clip while fewer than H/2 keys
// are held, so band tiles never crowd out the water's own and probe runs stay
// short. A band tile past the guard is not inserted and sets the table-full
// bit; one past the clip is not inserted at all.
@compute @workgroup_size(256)
fn wt_dilate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if wt_off() {
        return;
    }
    let side = wt_side();
    let cube = side * side * side;
    let j = gid.x / cube;
    if j >= atomicLoad(&wt_build[wt_seeds_word()]) {
        return;
    }
    let d = wt_offset(gid.x % cube);
    let t = wt_seed_tile(j) + d;
    if all(d == vec3<i32>(0)) || !wt_in_clip(t) {
        return;
    }
    if atomicLoad(&wt_build[wt_keys_word()]) >= wt.entries / 2u {
        wt_flag(WT_TABLE_FULL);
        return;
    }
    wt_insert(ts_tile_key(t));
}

// The same threads as wt_dilate, the seed's own tile among them: look each
// tile up, lower its ring to the Chebyshev tile distance from the seed, and
// write its tile.
@compute @workgroup_size(256)
fn wt_rings(@builtin(global_invocation_id) gid: vec3<u32>) {
    if wt_off() {
        return;
    }
    let side = wt_side();
    let cube = side * side * side;
    let j = gid.x / cube;
    if j >= atomicLoad(&wt_build[wt_seeds_word()]) {
        return;
    }
    let d = wt_offset(gid.x % cube);
    let t = wt_seed_tile(j) + d;
    let e = ts_entry_of(ts_tile_key(t));
    if e == WT_NONE {
        return;
    }
    let a = abs(d);
    atomicMin(&wt_entry[wt_ring_word(e)], u32(max(max(a.x, a.y), a.z)));
    _ = wt_claim(e, t);
}

// One thread a point in cell c: lower the near word of every tile within
// cell_reach cells of c to the Chebyshev cell distance from c to the tile's
// box. A tile's value is then the distance to its nearest point-holding cell
// within reach, cell_reach + 1 when none.
@compute @workgroup_size(256)
fn wt_near(@builtin(global_invocation_id) gid: vec3<u32>) {
    if wt_off() || !wt_live(gid.x) {
        return;
    }
    let c = wt_point_cell(gid.x);
    let reach = vec3<i32>(i32(wt.cell_reach));
    let first = (c - reach) >> vec3<u32>(3u);
    let last = (c + reach) >> vec3<u32>(3u);
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let q = vec3<i32>(x, y, z);
                let e = ts_entry_of(ts_tile_key(q));
                if e == WT_NONE {
                    continue;
                }
                let lo = q * 8;
                let d = max(max(lo - c, c - (lo + vec3<i32>(7))), vec3<i32>(0));
                atomicMin(&wt_entry[wt_near_word(e)], u32(max(max(d.x, d.y), d.z)));
            }
        }
    }
}

// P threads: a slot held last build looks its tile up. Found with ring <=
// alloc_ring (and holding that very tile, not an alias), the entry takes the
// slot back; otherwise the slot is freed and flagged for the scan. From
// scratch every slot is freed and none flagged: wt_free refills the stack.
@compute @workgroup_size(256)
fn wt_carry(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if wt_off() || s >= wt.pool_tiles {
        return;
    }
    let row = wt_slot_row(s);
    wt_scan[s] = 0u;
    wt_slot[wt_state_word(s)] = WT_FREE;
    if atomicLoad(&wt_table[row + 3u]) == WT_NONE {
        return;
    }
    if wt.from_scratch == 0u {
        let t = ts_slot_tile(s);
        let e = ts_entry_of(ts_tile_key(t));
        if e != WT_NONE && atomicLoad(&wt_entry[wt_ring_word(e)]) <= wt.alloc_ring
            && all(wt_entry_tile(e) == t) {
            atomicStore(&wt_table[wt_entry_slot_word(e)], s);
            atomicStore(&wt_table[row + 3u], e);
            wt_slot[wt_state_word(s)] = WT_CARRIED;
            atomicAdd(&wt_build[wt_hist_word(wt_entry_rank(e))], 1u);
            return;
        }
        wt_scan[s] = 1u;
    }
    atomicStore(&wt_table[row + 3u], WT_NONE);
}

// What entry e asks of the pool: 0 a slot for a tile of the cell set C
// (rank 0: a point's tile or one its stencil reaches), 1 a slot for the rest
// up to alloc_ring, 2 nothing (empty, already holding a slot, or past
// alloc_ring). A short pool so fills C first, and no empty band tile takes
// the slot a tile holding points needs.
fn wt_want(e: u32) -> u32 {
    if atomicLoad(&wt_table[wt_key_word(e)]) == WT_EMPTY
        || atomicLoad(&wt_table[wt_entry_slot_word(e)]) != WT_NONE {
        return 2u;
    }
    let ring = atomicLoad(&wt_entry[wt_ring_word(e)]);
    if ring > wt.alloc_ring {
        return 2u;
    }
    return select(1u, 0u, wt_entry_rank(e) == 0u);
}

// H threads: each entry's wants into the scan's two want sections.
@compute @workgroup_size(256)
fn wt_wants(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if wt_off() || e >= wt.entries {
        return;
    }
    let want = wt_want(e);
    wt_scan[wt.pool_tiles + e] = select(0u, 1u, want == 0u);
    wt_scan[wt.pool_tiles + wt.entries + e] = select(0u, 1u, want == 1u);
}

// The free stack's live length after the frees: the last build's top plus
// the freed slots, or the whole pool from scratch.
fn wt_available(freed: u32) -> u32 {
    if wt.from_scratch != 0u {
        return wt.pool_tiles;
    }
    return atomicLoad(&wt_counts[wt_free_top_word()]) + freed;
}

// P threads, after the scan: every freed slot pushed onto the free stack in
// slot order (the scan gives each its place above the last build's top).
// From scratch the stack is the whole pool, slot 0 on top.
@compute @workgroup_size(256)
fn wt_free(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if wt_off() || s >= wt.pool_tiles {
        return;
    }
    if wt.from_scratch != 0u {
        wt_slot[wt_stack_word(s)] = wt.pool_tiles - 1u - s;
        return;
    }
    var before = 0u;
    if s > 0u {
        before = wt_scan[s - 1u];
    }
    let place = wt_scan[s];
    if place != before {
        wt_slot[wt_stack_word(atomicLoad(&wt_counts[wt_free_top_word()]) + place - 1u)] = s;
    }
}

// H threads, after wt_free: every entry wanting a slot pops one, in table
// order with the cell set C first (its place among the wants from the scan). An
// entry finding the stack empty stays absent and sets the pool-short bit. A
// popped slot takes the entry's tile and becomes fresh.
@compute @workgroup_size(256)
fn wt_allocate(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if wt_off() || e >= wt.entries {
        return;
    }
    let want = wt_want(e);
    if want == 2u {
        return;
    }
    let p = wt.pool_tiles;
    let freed = wt_scan[p - 1u];
    let avail = wt_available(freed);
    let j = wt_scan[p + want * wt.entries + e] - freed - 1u;
    if j >= avail {
        wt_flag(WT_POOL_SHORT);
        return;
    }
    let slot = wt_slot[wt_stack_word(avail - 1u - j)];
    atomicStore(&wt_table[wt_entry_slot_word(e)], slot);
    let row = wt_slot_row(slot);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        atomicStore(&wt_table[row + axis], atomicLoad(&wt_entry[wt_coord_word(e, axis)]));
    }
    atomicStore(&wt_table[row + 3u], e);
    wt_slot[wt_state_word(slot)] = WT_FRESH;
    atomicAdd(&wt_build[wt_hist_word(wt_entry_rank(e))], 1u);
}

// P threads: each held slot's neighbour row (26 hash lookups), its rank, and
// its place in the lists: by rank, retired (carried out of C) and fresh. A
// free slot's row is all WT_NONE.
@compute @workgroup_size(256)
fn wt_slots(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if wt_off() || s >= wt.pool_tiles {
        return;
    }
    let rows = wt_slot_row(wt.pool_tiles) + WT_NEIGHBOURS * s;
    let e = atomicLoad(&wt_table[wt_slot_row(s) + 3u]);
    if e == WT_NONE {
        for (var k = 0u; k < WT_NEIGHBOURS; k = k + 1u) {
            atomicStore(&wt_table[rows + k], WT_NONE);
        }
        return;
    }
    let t = ts_slot_tile(s);
    for (var k = 0u; k < WT_NEIGHBOURS; k = k + 1u) {
        let d = vec3<i32>(i32(k % 3u), i32((k / 3u) % 3u), i32(k / 9u)) - vec3<i32>(1);
        var slot = s;
        if k != 13u {
            slot = ts_hash_slot(t + d);
        }
        atomicStore(&wt_table[rows + k], slot);
    }
    let rank = wt_entry_rank(e);
    let before = wt_slot[wt_rank_word(s)];
    let state = wt_slot[wt_state_word(s)];
    wt_slot[wt_rank_word(s)] = rank;
    var base = 0u;
    for (var k = 0u; k < rank; k = k + 1u) {
        base = base + atomicLoad(&wt_build[wt_hist_word(k)]);
    }
    wt_by_rank[base + atomicAdd(&wt_build[wt_cursor_word(rank)], 1u)] = s;
    if state == WT_CARRIED && before == 0u && rank != 0u {
        wt_retired[atomicAdd(&wt_build[wt_retired_cursor()], 1u)] = s;
    }
    if state == WT_FRESH {
        wt_fresh[atomicAdd(&wt_build[wt_fresh_cursor()], 1u)] = s;
    }
}

fn wt_triple(word: u32, slots: u32) {
    wt_args[word] = 2u * slots;
    wt_args[word + 1u] = 1u;
    wt_args[word + 2u] = 1u;
}

// One thread, last: the counts, the free stack's new top and the indirect
// triples (512 threads a slot, 256 a group), and the dilation's triple back
// to no groups. Runs on an inactive clock slot too, to switch every list
// triple off and leave everything else as the last active build left it.
@compute @workgroup_size(1)
fn wt_finish() {
    let r = wt.ring_max;
    let range = wt_range_word();
    let fresh = range + 2u;
    let running = !wt_off();
    if range != 3u * (r + 2u) {
        wt_args[3u * (r + 2u)] = 0u;
    }
    wt_args[range] = 0u;
    wt_args[range + 1u] = select(0u, wt.replay_first, running);
    wt_args[range + 6u] = 0u;
    wt_args[range + 7u] = select(0u, wt.replay_second, running);
    atomicStore(&wt_build_args[0], 0u);
    if !running {
        for (var k = 0u; k <= r + 1u; k = k + 1u) {
            wt_args[3u * k] = 0u;
        }
        wt_args[fresh] = 0u;
        return;
    }
    var held = 0u;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        held = held + atomicLoad(&wt_build[wt_hist_word(k)]);
        atomicStore(&wt_counts[k], held);
    }
    let retired = atomicLoad(&wt_build[wt_retired_cursor()]);
    let fresh_count = atomicLoad(&wt_build[wt_fresh_cursor()]);
    atomicStore(&wt_counts[wt_retired_word()], retired);
    atomicStore(&wt_counts[wt_fresh_word()], fresh_count);
    let p = wt.pool_tiles;
    let freed = wt_scan[p - 1u];
    let wanted = wt_scan[p + 2u * wt.entries - 1u] - freed;
    let avail = wt_available(freed);
    atomicStore(&wt_counts[wt_free_top_word()], avail - min(wanted, avail));
    for (var k = 0u; k <= r; k = k + 1u) {
        wt_triple(3u * k, atomicLoad(&wt_counts[k]));
    }
    wt_triple(3u * (r + 1u), retired);
    wt_triple(fresh, fresh_count);
}

// Whether the shared table has this table's H. A table whose pool just
// changed is rebuilt at a new H; until both sides have it, neither shares.
fn wt_sharing() -> bool {
    return atomicLoad(&wt_shared[0]) == wt.entries;
}

// Whether slot s of the shared table is held: a slot of its pool (P from its
// header, whatever the dispatch was sized for) whose row has an entry.
fn wt_shared_held(s: u32) -> bool {
    return s < atomicLoad(&wt_shared[1]) && atomicLoad(&wt_shared[wt_slot_row(s) + 3u]) != WT_NONE;
}

fn wt_shared_tile(s: u32) -> vec3<i32> {
    let row = wt_slot_row(s);
    return bitcast<vec3<i32>>(vec3<u32>(
        atomicLoad(&wt_shared[row]),
        atomicLoad(&wt_shared[row + 1u]),
        atomicLoad(&wt_shared[row + 2u]),
    ));
}

// One thread a slot of the shared table, after wt_seed: each held tile's key
// joins this table's keys. Its entry gets no ring, so it wants no slot here.
@compute @workgroup_size(256)
fn wt_foreign(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if wt_off() || !wt_sharing() || !wt_shared_held(s) {
        return;
    }
    wt_insert(ts_tile_key(wt_shared_tile(s)));
}

// H threads, after wt_finish: the shared table takes this table's keys, and
// every entry loses its slot there until wt_repoint gives it back.
@compute @workgroup_size(256)
fn wt_share(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if wt_off() || e >= wt.entries || !wt_sharing() {
        return;
    }
    atomicStore(&wt_shared[wt_key_word(e)], atomicLoad(&wt_table[wt_key_word(e)]));
    atomicStore(&wt_shared[wt_entry_slot_word(e)], WT_NONE);
}

// One thread a slot of the shared table, after wt_share: each held slot finds
// its tile's entry among the keys both tables now hold and takes it back. A
// tile whose key a full table dropped keeps its old entry and sets the
// table-full bit; lookups miss it until the shared table's next build frees
// the slot.
@compute @workgroup_size(256)
fn wt_repoint(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if wt_off() || !wt_sharing() || !wt_shared_held(s) {
        return;
    }
    let e = ts_entry_of(ts_tile_key(wt_shared_tile(s)));
    if e == WT_NONE {
        wt_flag(WT_TABLE_FULL);
        return;
    }
    atomicStore(&wt_shared[wt_entry_slot_word(e)], s);
    atomicStore(&wt_shared[wt_slot_row(s) + 3u], e);
}
