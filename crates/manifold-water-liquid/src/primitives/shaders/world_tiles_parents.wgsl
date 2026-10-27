// The points of a parents build (WATER_INFINITE_DOMAIN_DESIGN.md section 7):
// a coarse level's tiles are the parents (tile >> 1) of the level below's
// held tiles. Point i is the level below's slot i, read from its table at
// binding 13 (its H and P are `child_entries` and `child_pool_tiles`); its
// cell is the first cell of its parent tile, so with cell_reach 0 every
// parent tile is rank 0. Adds the children rows (`wt_children`).

@group(0) @binding(13) var<storage, read> wt_child: array<u32>;
// Per slot, the slots of its 8 children in the level below, child c at
// offset (c & 1, (c >> 1) & 1, c >> 2) from twice its tile, WT_NONE where
// absent or the slot is free.
@group(0) @binding(14) var<storage, read_write> wt_child_rows: array<u32>;

fn wt_child_row(slot: u32) -> u32 {
    return 2u * wt.child_entries + WT_SLOT_WORDS * slot;
}

fn wt_child_tile(slot: u32) -> vec3<i32> {
    let row = wt_child_row(slot);
    return bitcast<vec3<i32>>(vec3<u32>(wt_child[row], wt_child[row + 1u], wt_child[row + 2u]));
}

fn wt_point_live(i: u32) -> bool {
    return i < wt.point_count && wt_child[wt_child_row(i) + 3u] != WT_NONE;
}

fn wt_point_cell(i: u32) -> vec3<i32> {
    return (wt_child_tile(i) >> vec3<u32>(1u)) * 8;
}

// The level below's slot of tile t, WT_NONE when absent: the read side's
// `ts_hash_slot`, over the child table.
fn wt_child_slot(t: vec3<i32>) -> u32 {
    let entries = wt.child_entries;
    let key = ts_tile_key(t);
    var i = ts_hash(key, entries);
    for (var walked = 0u; walked < entries; walked = walked + 1u) {
        let c = wt_child[i];
        if c == key {
            let slot = wt_child[entries + i];
            if slot == WT_NONE || any(wt_child_tile(slot) != t) {
                return WT_NONE;
            }
            return slot;
        }
        if c < key {
            return WT_NONE;
        }
        i = (i + 1u) & (entries - 1u);
    }
    return WT_NONE;
}

// P threads, after wt_slots: every slot's children row.
@compute @workgroup_size(256)
fn wt_children(@builtin(global_invocation_id) gid: vec3<u32>) {
    let s = gid.x;
    if wt_off() || s >= wt.pool_tiles {
        return;
    }
    let held = atomicLoad(&wt_table[wt_slot_row(s) + 3u]) != WT_NONE;
    let first = ts_slot_tile(s) * 2;
    for (var c = 0u; c < 8u; c = c + 1u) {
        var slot = WT_NONE;
        if held {
            let o = vec3<i32>(vec3<u32>(c & 1u, (c >> 1u) & 1u, c >> 2u));
            slot = wt_child_slot(first + o);
        }
        wt_child_rows[8u * s + c] = slot;
    }
}
