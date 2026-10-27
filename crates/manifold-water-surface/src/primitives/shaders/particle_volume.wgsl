// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// node.particle_volume — one workgroup per slot of the display's world tiles
// (`buf_tiles`, `pool_tiles` tiles), and one more for the canonical element.
// Element p is display node p, at (p - 1/2) cells, the low corner of cell p.
// Negative-inside port of FLIP Fluids ParticleMesher: the value starts at
// 3 * blob_reach (Particle Scale times the largest particle radius) and takes
// the nearest kernel sphere's |x - c| - r over every blob whose inclusive
// floor(c - 1.5r)..floor(c + 1.5r)+1 grid box holds the node.
//
// The blobs are sorted into the display cells (`cell_ranges`, one range per
// pool element). A node searches the cells from floor(node - 1 - u) to
// floor(node + 1 + u), u being 1.5 * blob_reach in cells, which hold every
// blob that can reach it; a larger blob is cut where its support leaves the
// searched cells. The workgroup walks the union of its tile's windows once:
// each blob lowers the nodes of the tile its box holds and whose window holds
// its cell, through an order-preserving key in workgroup memory, so the
// minimum is exact whatever order the blobs land in. A node inside a solid
// (`solid` is a node array on the same tiles) is never inside the liquid. A
// free slot and the canonical element hold the far value, FIELD_FAR_CELLS (3)
// cells.

struct Params {
    cell_size: f32,
    blob_reach: f32,
    pool_tiles: u32,
    entries: u32,
}

struct CellRange {
    start: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> blobs: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read> solid: array<f32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> levelset: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.entries; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

const PV_THREADS: u32 = 256u;
// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const PV_FAR_CELLS: f32 = 3.0;
// The most cells a tile's window spans on an axis: the host refuses more than
// 16 cells of support, whose window spans at most 8 + 2 * 17 + 1.
const PV_MAX_WINDOW: i32 = 48;

var<workgroup> pv_best: array<atomic<u32>, 512>;
var<workgroup> pv_held: u32;

// A key whose unsigned order is the float order: the sign bit set for
// positives, every bit flipped for negatives.
fn pv_key(v: f32) -> u32 {
    let b = bitcast<u32>(v);
    return select(b | 0x80000000u, ~b, (b & 0x80000000u) != 0u);
}

fn pv_value(key: u32) -> f32 {
    return bitcast<f32>(select(~key, key & 0x7fffffffu, (key & 0x80000000u) != 0u));
}

// The first and last cell node n searches along one axis. The slack covers
// rounding in the sort's cell product and in the support test; it only
// widens the search.
fn pv_window(n: i32, u: f32) -> vec2<i32> {
    let q = f32(n);
    let slack = 1.0e-6 * (abs(q) + (u + 2.0));
    return vec2<i32>(i32(floor(q - (u + 1.0) - slack)), i32(floor(q + (u + 1.0) + slack)));
}

fn pv_searches(n: i32, k: i32, u: f32) -> bool {
    let w = pv_window(n, u);
    return w.x <= k && k <= w.y;
}

// The nodes along one axis that a blob in cell k with support box
// first..last lowers: inside the tile from t0, inside the box, and searching
// cell k. Windows move up with the node, so those nodes are one run.
fn pv_run(first: i32, last: i32, t0: i32, k: i32, u: f32) -> vec2<i32> {
    var a = max(first, t0);
    var b = min(last, t0 + 7);
    loop {
        if a > b || pv_searches(a, k, u) {
            break;
        }
        a = a + 1;
    }
    loop {
        if b < a || pv_searches(b, k, u) {
            break;
        }
        b = b - 1;
    }
    return vec2<i32>(a, b);
}

@compute @workgroup_size(256)
fn cs_main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    let h = params.cell_size;
    let far = PV_FAR_CELLS * h;
    let reach = params.blob_reach;
    if lane == 0u {
        pv_held = u32(slot < params.pool_tiles && ts_slot_entry(slot) != WT_NONE);
        if slot == params.pool_tiles {
            levelset[ts_canon()] = far;
        }
    }
    let band = pv_key(3.0 * reach);
    atomicStore(&pv_best[lane], band);
    atomicStore(&pv_best[lane + PV_THREADS], band);
    if workgroupUniformLoad(&pv_held) == 0u {
        if slot < params.pool_tiles {
            levelset[slot * 512u + lane] = far;
            levelset[slot * 512u + lane + PV_THREADS] = far;
        }
        return;
    }
    ts_home(slot * 512u);
    let t0 = ts_home_tile * 8;
    let u = 1.5 * reach / h;
    let low = vec3<i32>(pv_window(t0.x, u).x, pv_window(t0.y, u).x, pv_window(t0.z, u).x);
    let high = vec3<i32>(pv_window(t0.x + 7, u).y, pv_window(t0.y + 7, u).y, pv_window(t0.z + 7, u).y);
    let span = clamp(high - low + vec3<i32>(1), vec3<i32>(0), vec3<i32>(PV_MAX_WINDOW));
    let cells = u32(span.x * span.y * span.z);
    let canon = ts_canon();
    for (var i = lane; i < cells; i = i + PV_THREADS) {
        let k = low + vec3<i32>(i32(i) % span.x, (i32(i) / span.x) % span.y, i32(i) / (span.x * span.y));
        let e = ts_cell_at(k);
        if e >= canon {
            continue;
        }
        let range = cell_ranges[e];
        for (var b = range.start; b < range.start + range.count; b = b + 1u) {
            let blob = blobs[b];
            let r = blob.w;
            if !(r > 0.0) {
                continue;
            }
            // Native support is a grid-aligned box including its outer
            // interpolation node. Spherical rejection loses the corners.
            let support = 1.5 * r;
            let first = vec3<i32>(floor((blob.xyz - vec3<f32>(support)) / h + vec3<f32>(0.5)));
            let last = vec3<i32>(floor((blob.xyz + vec3<f32>(support)) / h + vec3<f32>(0.5))) + vec3<i32>(1);
            let rx = pv_run(first.x, last.x, t0.x, k.x, u);
            let ry = pv_run(first.y, last.y, t0.y, k.y, u);
            let rz = pv_run(first.z, last.z, t0.z, k.z, u);
            for (var z = rz.x; z <= rz.y; z = z + 1) {
                for (var y = ry.x; y <= ry.y; y = y + 1) {
                    for (var x = rx.x; x <= rx.y; x = x + 1) {
                        let node = vec3<i32>(x, y, z);
                        let p = (vec3<f32>(node) - vec3<f32>(0.5)) * h;
                        let local = ts_local(node);
                        atomicMin(&pv_best[local], pv_key(length(p - blob.xyz) - r));
                    }
                }
            }
        }
    }
    workgroupBarrier();
    for (var l = lane; l < 512u; l = l + PV_THREADS) {
        let e = slot * 512u + l;
        var phi = pv_value(atomicLoad(&pv_best[l]));
        if solid[e] < 0.0 {
            phi = max(phi, 0.0);
        }
        levelset[e] = phi;
    }
}
