// The kept points of a world tiles build (world_tiles.rs `Allocation::kept`):
// one word per slot of the table as the last build left it, the local index
// of a cell the slot's tile keeps (GPU FLIP's narrow band: the lowest cell
// its grid interior holds water in), WT_NONE for none. Point i is slot i's
// kept cell. The build reads the slot rows before wt_slots rewrites them, so
// a kept slot's tile is the one it held when the word was written.

@group(0) @binding(13) var<storage, read> wt_kept: array<u32>;

fn wt_point_live(i: u32) -> bool {
    return i < wt.pool_tiles && wt_kept[i] != WT_NONE && ts_slot_entry(i) != WT_NONE;
}

fn wt_point_cell(i: u32) -> vec3<i32> {
    return ts_slot_tile(i) * 8 + ts_local_coord(wt_kept[i]);
}
