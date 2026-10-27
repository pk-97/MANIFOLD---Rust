var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.field_smooth_blend — fusable BUFFER body. One thread per pool element
// of the display's world tiles (`buf_tiles`, `pool_tiles` tiles): the
// weighted, leashed blend of a smoothed level set toward its prior, held
// within a leash of the reference, the particles' level set. `smoothed` is this element's
// own value; `prior`, `weights` and `reference` are read at the same pool
// index. cell_size is the lattice spacing in metres and the band is in cells.
// The leash is Smoothness times the field build's half-cell leash
// (Smoothness 1 is the tuned surface). The canonical element and a free slot
// hold the canonical level set.
const FSB_LEASH_CELLS: f32 = 0.5; // field_build::FIELD_LEASH_CELLS
fn body(idx: u32, count: u32, e_smoothed: f32, cell_size: f32, smoothness: f32, band_cells: f32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    // FIELD_FAR_CELLS (scene_object) cells: the level set where no tile is stored.
    if idx >= ts_canon() || ts_slot_entry(idx >> 9u) == WT_NONE { return 3.0 * cell_size; }
    let prior = buf_prior[idx];
    let reference = buf_reference[idx];
    if abs(reference) >= band_cells * cell_size { return prior; }
    let leash = smoothness * FSB_LEASH_CELLS * cell_size;
    return clamp(prior + buf_weights[idx] * (e_smoothed - prior), reference - leash, reference + leash);
}
