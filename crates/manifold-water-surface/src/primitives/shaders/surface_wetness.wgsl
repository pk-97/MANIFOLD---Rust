// Surface wetness: per display node, how wet the surface is there, 0 to 1,
// stored per element of the display's world tiles (composed after the read
// side, `manifold_node_engine::scene::world_tiles::READ_SIDE`).
//
// Where the water's level set is below one display cell (water at the node or
// within a cell of it) the node is wet. Elsewhere its wetness is the last
// frame's times `decay`, the fraction of the film still on after the time
// since then. The buffer is the state, so `wetness` is both read and
// written, and it is stored per slot: a slot that holds another tile than it
// did last frame, or was free, has nothing to carry (`last_rows` is last
// frame's slot rows) and starts dry. A free slot and the canonical element
// read 0, the wetness of a tile the table does not hold.

struct Params {
    pool_tiles: u32,
    elements: u32,
    h: f32,
    decay: f32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> field: array<f32>;
@group(0) @binding(2) var<storage, read> tiles: array<u32>;
@group(0) @binding(3) var<storage, read> last_rows: array<u32>;
@group(0) @binding(4) var<storage, read_write> wetness: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(params.pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

@compute @workgroup_size(64)
fn cs_wetness(@builtin(global_invocation_id) gid: vec3<u32>) {
    let e = gid.x;
    if e >= params.elements {
        return;
    }
    let slot = e >> 9u;
    if e >= ts_canon() || ts_slot_entry(slot) == WT_NONE {
        wetness[e] = 0.0;
        return;
    }
    let now = 2u * ts_table_entries() + WT_SLOT_WORDS * slot;
    let before = WT_SLOT_WORDS * slot;
    let same = last_rows[before + 3u] != WT_NONE
        && last_rows[before] == tiles[now]
        && last_rows[before + 1u] == tiles[now + 1u]
        && last_rows[before + 2u] == tiles[now + 2u];
    let carried = select(0.0, wetness[e], same);
    wetness[e] = select(carried * params.decay, 1.0, field[e] < params.h);
}
