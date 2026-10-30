// What each held tile of a water field's table holds, so the march and the
// mist step over the tiles they have nothing to find in as they step over an
// unheld one. Starts after `world_tiles_read.wgsl`, `field_tiles.wgsl` and
// `water_march_core.wgsl` (the uniforms, the level set and the TILE_ bits).
// One workgroup per pool slot reads the nodes of every cell whose lower
// corner is in the tile, the tile's own and its far faces': TILE_WATER when
// the level set is at or below zero at one, TILE_AIR when it is at or above
// zero at one, TILE_MIST when the mist is above zero at one (with
// `u.flags.y` set, a mist array bound). A free slot gets no bits.

@group(0) @binding(7) var<storage, read> mist: array<f32>;
@group(0) @binding(72) var<storage, read_write> tile_flags: array<u32>;

// The march core asks for it; this pass marches nothing.
fn field_tile_dry(slot: u32) -> bool {
    return false;
}

// The nodes a tile's cells interpolate: 9 per axis.
const FLAG_NODES: u32 = 729u;
const FLAG_THREADS: u32 = 256u;

var<workgroup> wg_held: u32;
var<workgroup> wg_bits: atomic<u32>;

@compute @workgroup_size(256)
fn cs_tile_flags(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    field_pool = u.tiles.x;
    if lane == 0u {
        wg_held = u32(ts_slot_entry(slot) != WT_NONE);
        atomicStore(&wg_bits, 0u);
    }
    if workgroupUniformLoad(&wg_held) == 0u {
        if lane == 0u {
            tile_flags[slot] = 0u;
        }
        return;
    }
    ts_home(slot * 512u);
    let base = ts_home_tile * 8;
    var bits = 0u;
    for (var i = lane; i < FLAG_NODES; i = i + FLAG_THREADS) {
        let e = ts_cell_at(base + vec3<i32>(i32(i % 9u), i32((i / 9u) % 9u), i32(i / 81u)));
        let phi = levelset[e];
        bits = bits | select(0u, TILE_WATER, phi <= 0.0) | select(0u, TILE_AIR, phi >= 0.0);
        if u.flags.y != 0u && mist[e] > 0.0 {
            bits = bits | TILE_MIST;
        }
    }
    if bits != 0u {
        atomicOr(&wg_bits, bits);
    }
    workgroupBarrier();
    if lane == 0u {
        tile_flags[slot] = atomicLoad(&wg_bits);
    }
}
