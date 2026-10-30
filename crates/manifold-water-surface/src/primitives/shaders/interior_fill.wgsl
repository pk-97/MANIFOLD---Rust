// node.interior_fill (interior_fill.rs): the display's level set with GPU
// FLIP Narrow Band's grid water filled in. Composed after the display table's
// read side and the simulation table's (the same read side renamed sim_ts_ /
// SIM_WT_). One thread per display pool element.

struct Params {
    // The display cell size and the simulation's.
    cell: f32,
    sim_cell: f32,
    pool_tiles: u32,
    sim_pool_tiles: u32,
    _pad_h: u32,
    _pad_sim_h: u32,
    // Metres the grid water is shrunk by before it fills.
    shrink: f32,
    elements: u32,
}

@group(0) @binding(0) var<uniform> u: Params;
// The display's level set, a node array on `tiles`.
@group(0) @binding(1) var<storage, read> levelset: array<f32>;
// The grid interior, a cell array on `interior_tiles`; its canonical element
// is air.
@group(0) @binding(2) var<storage, read> interior: array<f32>;
@group(0) @binding(3) var<storage, read> interior_tiles: array<u32>;
@group(0) @binding(4) var<storage, read> tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> out: array<f32>;

fn ts_pool_tiles() -> u32 { return u.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }
fn sim_ts_pool_tiles() -> u32 { return u.sim_pool_tiles; }
fn sim_ts_table_word(i: u32) -> u32 { return interior_tiles[i]; }

// A free slot and the canonical element keep the level set's value. Display
// node p sits at (p − ½)·h_s and simulation cell c is centred on c·h, so the
// node reads the interior trilinearly at (p − ½)·h_s / h.
@compute @workgroup_size(256)
fn cs_fill(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.elements {
        return;
    }
    let value = levelset[idx];
    if idx >= ts_canon() || ts_slot_entry(idx >> 9u) == WT_NONE {
        out[idx] = value;
        return;
    }
    ts_home(idx);
    let p = ts_cell_coord(idx);
    let s = (vec3<f32>(p) - vec3<f32>(0.5)) * (u.cell / u.sim_cell);
    let base = vec3<i32>(floor(s));
    let t = s - vec3<f32>(base);
    sim_ts_home(sim_ts_cell_at(base));
    var grid = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
        let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
        grid = grid + w3.x * w3.y * w3.z * interior[sim_ts_cell_at(base + bit)];
    }
    out[idx] = min(value, grid + u.shrink);
}
