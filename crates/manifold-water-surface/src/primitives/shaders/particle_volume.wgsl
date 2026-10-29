// Uses the search-radius ratio (1.5 radii) from FLIP Fluids particlemesher.cpp `_searchRadiusFactor` (MIT); see THIRD_PARTY_NOTICES.md.
// node.particle_volume — one workgroup per slot of the display's world tiles
// (`buf_tiles`, `pool_tiles` tiles), and one more for the canonical element.
// Element p is display node p, at (p - 1/2) cells, the low corner of cell p.
// Blended stretched blobs (Zhu and Bridson 2005, "Animating sand as a
// fluid"): each blob carries a symmetric shape matrix A, and a point at offset
// y from its centre is inside it when |A y| < r. The blob's stretch, size · A
// (its shrink undone), stretches its kernel: over the blobs within R = 1.5 *
// blob_reach (Particle Scale times the largest particle radius) of node x,
// weighted w = (1 - (|size · A y| / R)^2)^3, y the offset from x to the
// blob's centre, the blend runs on each blob's stretched offset q = A y, its
// radius r and its gradient scale g = |A q| / |q| (how fast |A y| grows
// there): phi = (|q̄| - r̄) / ḡ, the blended ellipsoid's algebraic distance
// over its gradient, negative inside. For round blobs (A = I) that is
// Zhu and Bridson's |x - c̄| - r̄ exactly, and where neighbouring blobs share
// a shape it is the blend of their mean ellipsoid, at six sums a node instead
// of the eleven a blended shape matrix takes. Two blobs up to 2R apart blend
// into one body where
// their kernels alone would leave a gap, so a thin spread of particles reads
// as a film, and a lone blob is its own ellipsoid. A node that gathers less
// than PV_MIN_WEIGHT reads 3 * blob_reach. A blob visits the nodes of its
// inclusive floor(c - R')..floor(c + R')+1 grid box, R' = R times its
// weight's reach (shape_diagonal.w).
//
// The same blend gives each node how much the field smoothing moves it and
// the water's velocity there: `weights` fades the weighted mean of the
// blobs' neighbour counts (`counts`, from node.shape_particle_blobs) from
// fade_start to fade_end, 1 in bulk water and 0 on thin sheets and drops,
// and `velocity` (xyz at 3p..3p+2) is the weighted mean of their particles'
// velocities (`sorted`). Both reach every node the level set does, at every
// Surface Detail. A node no blob reaches, a free slot and the canonical
// element hold 0 in both.
//
// The blobs are sorted into the display cells (`cell_ranges`, one range per
// pool element) by their particles, and a blob's centre may sit off its
// particle. A node searches the cells from floor(node - 1 - u) to
// floor(node + 1 + u), u being `window` in cells (the most stretched blob's
// support plus the farthest a centre moves), which hold every blob that can
// reach it. The workgroup walks the union of its tile's windows once, a run
// of up to `run` cells along x at a time (about the same blobs a run at every
// Surface Detail): the sort keeps a tile's blobs in cell order, so a run
// inside one tile is one run of blobs, read from two ranges, and
// each window tile's slot is looked up once into workgroup memory. Every
// blob adds to the nodes of the tile its box holds (the window holds every
// blob that reaches them, so each pair is counted once whichever tile walks
// it), through fixed-point sums in workgroup memory, so the result is exact
// whatever order the blobs land in. A node inside a solid
// (`solid`, the solid's distance on the same tiles, negative inside) takes at
// least its depth in the solid less PV_WALL_REACH cells: water against a wall
// reaches half a cell into it, so the water's surface meets the wall's face
// at an angle behind the face instead of lying on it, where the two would
// draw in patches over each other; deeper in, no node through the wall reads
// as near the water. A free slot and the canonical element hold the far
// value, FIELD_FAR_CELLS (3) cells.

// How far, in cells, water reaches into a solid.
const PV_WALL_REACH: f32 = 0.5;

struct Params {
    cell_size: f32,
    blob_reach: f32,
    pool_tiles: u32,
    entries: u32,
    // How far a blob reaches from its particle's cell, metres.
    window: f32,
    // Cells per run of the walk, 1 to 8.
    run: u32,
    // The mean neighbour count at which smoothing starts, and where it is full.
    fade_start: f32,
    fade_end: f32,
}

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

// Field order identical to the host's `FluidBlob`: centre xyz and radius, the
// shape matrix's diagonal and the weight's reach over R, its xy, xz, yz and
// the blob's size.
struct FluidBlob {
    center_radius: vec4<f32>,
    shape_diagonal: vec4<f32>,
    shape_off_diagonal: vec4<f32>,
}

struct CellRange {
    start: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> blobs: array<FluidBlob>;
@group(0) @binding(2) var<storage, read> cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read> solid: array<f32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> levelset: array<f32>;
@group(0) @binding(6) var<storage, read> counts: array<f32>;
@group(0) @binding(7) var<storage, read> sorted: array<FluidParticle>;
@group(0) @binding(8) var<storage, read_write> weights: array<f32>;
@group(0) @binding(9) var<storage, read_write> velocity: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.entries; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

const PV_THREADS: u32 = 256u;
// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const PV_FAR_CELLS: f32 = 3.0;
// The most cells a tile's window spans on an axis: the host refuses a window
// over 20 cells, which spans at most 8 + 2 * 21 + 1.
const PV_MAX_WINDOW: i32 = 52;

// Per node, ten fixed-point sums over the blobs within R of it: the weight,
// the weighted stretched offset q in R (xyz), the weighted radius in R, the
// weighted gradient scale, the weighted neighbour count and the weighted
// velocity (xyz).
const PV_SUMS: u32 = 10u;
var<workgroup> pv_sum: array<atomic<i32>, 5120>;
// The fixed-point scale of the sums. Each term is at most a few units (q and
// g reach 1 / size, about 4) and a node's weights sum to at most about a
// hundred blobs, far inside an i32.
const PV_FIXED: f32 = 1048576.0;
// The neighbour count's scale: a count reaches about sixty.
const PV_FIXED_COUNT: f32 = 16384.0;
// The velocity's scale, metres per second: a hundred blobs at 100 m/s stay
// far inside an i32, to a quarter millimetre per second.
const PV_FIXED_VELOCITY: f32 = 4096.0;
// The least weight a node must gather to read as near a blob: R * 0.95 from a
// lone one. Below it the rounded sums say nothing about where the blobs are,
// and the node reads as air.
const PV_MIN_WEIGHT: f32 = 1.0e-3;
var<workgroup> pv_held: u32;
// The slots of the tiles the window touches, x fastest, at most
// ceil(PV_MAX_WINDOW / 8) + 1 a side.
var<workgroup> pv_slots: array<u32, 512>;
const PV_MAX_TILES: i32 = 8;

// The first and last cell node n searches along one axis. The slack covers
// rounding in the sort's cell product and in the support test; it only
// widens the search.
fn pv_window(n: i32, u: f32) -> vec2<i32> {
    let q = f32(n);
    let slack = 1.0e-6 * (abs(q) + (u + 2.0));
    return vec2<i32>(i32(floor(q - (u + 1.0) - slack)), i32(floor(q + (u + 1.0) + slack)));
}

// The symmetric matrix with diagonal d and off-diagonals (xy, xz, yz) o times v.
fn pv_apply(d: vec3<f32>, o: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        d.x * v.x + o.x * v.y + o.y * v.z,
        o.x * v.x + d.y * v.y + o.z * v.z,
        o.y * v.x + o.z * v.y + d.z * v.z,
    );
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
            let canon = ts_canon();
            levelset[canon] = far;
            weights[canon] = 0.0;
            velocity[3u * canon] = 0.0;
            velocity[3u * canon + 1u] = 0.0;
            velocity[3u * canon + 2u] = 0.0;
        }
    }
    let band = 3.0 * reach;
    for (var l = lane; l < 512u * PV_SUMS; l = l + PV_THREADS) {
        atomicStore(&pv_sum[l], 0);
    }
    // Zhu and Bridson's blend radius: the support every blob already visits.
    let blend = 1.5 * reach;
    if workgroupUniformLoad(&pv_held) == 0u {
        if slot < params.pool_tiles {
            for (var e = slot * 512u + lane; e < slot * 512u + 512u; e = e + PV_THREADS) {
                levelset[e] = far;
                weights[e] = 0.0;
                velocity[3u * e] = 0.0;
                velocity[3u * e + 1u] = 0.0;
                velocity[3u * e + 2u] = 0.0;
            }
        }
        return;
    }
    ts_home(slot * 512u);
    let t0 = ts_home_tile * 8;
    let u = params.window / h;
    let low = vec3<i32>(pv_window(t0.x, u).x, pv_window(t0.y, u).x, pv_window(t0.z, u).x);
    let high = vec3<i32>(pv_window(t0.x + 7, u).y, pv_window(t0.y + 7, u).y, pv_window(t0.z + 7, u).y);
    let span = clamp(high - low + vec3<i32>(1), vec3<i32>(0), vec3<i32>(PV_MAX_WINDOW));
    let tile_low = low >> vec3<u32>(3u);
    let tiles = clamp(
        ((low + span - vec3<i32>(1)) >> vec3<u32>(3u)) - tile_low + vec3<i32>(1),
        vec3<i32>(0),
        vec3<i32>(PV_MAX_TILES),
    );
    for (var i = i32(lane); i < tiles.x * tiles.y * tiles.z; i = i + i32(PV_THREADS)) {
        let t = vec3<i32>(i % tiles.x, (i / tiles.x) % tiles.y, i / (tiles.x * tiles.y));
        pv_slots[i] = ts_slot_of(tile_low + t);
    }
    workgroupBarrier();
    // One item per run of cells along x (runs never cross a tile) and cell
    // (y, z) of the window.
    let run = i32(params.run);
    let runs = (8 / run) * tiles.x;
    let items = runs * span.y * span.z;
    for (var i = i32(lane); i < items; i = i + i32(PV_THREADS)) {
        let piece = i % runs;
        let y = low.y + (i / runs) % span.y;
        let z = low.z + i / (runs * span.y);
        let tile = vec3<i32>(tile_low.x + piece / (8 / run), y >> 3u, z >> 3u);
        let t = tile - tile_low;
        let slot = pv_slots[t.x + tiles.x * (t.y + tiles.y * t.z)];
        if slot == WT_NONE {
            continue;
        }
        let run_x = tile.x * 8 + (piece % (8 / run)) * run;
        let x0 = max(low.x, run_x);
        let x1 = min(low.x + span.x - 1, run_x + run - 1);
        if x0 > x1 {
            continue;
        }
        let head = cell_ranges[slot * 512u + ts_local(vec3<i32>(x0, y, z))];
        let tail = cell_ranges[slot * 512u + ts_local(vec3<i32>(x1, y, z))];
        for (var b = head.start; b < tail.start + tail.count; b = b + 1u) {
            let centre_radius = blobs[b].center_radius;
            let centre = centre_radius.xyz;
            let r = centre_radius.w;
            if !(r > 0.0) {
                continue;
            }
            let shape_diagonal = blobs[b].shape_diagonal;
            // Native support is a grid-aligned box including its outer
            // interpolation node, cut to the tile.
            let support = blend * shape_diagonal.w;
            let first = vec3<i32>(floor((centre - vec3<f32>(support)) / h + vec3<f32>(0.5)));
            let last = vec3<i32>(floor((centre + vec3<f32>(support)) / h + vec3<f32>(0.5))) + vec3<i32>(1);
            let lo_node = max(first, t0);
            let hi_node = min(last, t0 + vec3<i32>(7));
            // Most blobs in the window miss the tile: only those that reach
            // it read their shape's off-diagonal.
            if any(lo_node > hi_node) {
                continue;
            }
            let diag = shape_diagonal.xyz;
            let shape_off_diagonal = blobs[b].shape_off_diagonal;
            let off = shape_off_diagonal.xyz;
            let size = shape_off_diagonal.w;
            let count = counts[b] * PV_FIXED_COUNT;
            let moving = sorted[b].velocity * PV_FIXED_VELOCITY;
            for (var nz = lo_node.z; nz <= hi_node.z; nz = nz + 1) {
                for (var ny = lo_node.y; ny <= hi_node.y; ny = ny + 1) {
                    for (var nx = lo_node.x; nx <= hi_node.x; nx = nx + 1) {
                        let node = vec3<i32>(nx, ny, nz);
                        let p = (vec3<f32>(node) - vec3<f32>(0.5)) * h;
                        let q = pv_apply(diag, off, centre - p);
                        let m = size * q;
                        let s2 = dot(m, m) / (blend * blend);
                        if s2 >= 1.0 {
                            continue;
                        }
                        let fall = 1.0 - s2;
                        let w = fall * fall * fall;
                        // The gradient scale |A q| / |q|; at the centre,
                        // where it has no direction, the mean axis.
                        let reach_q = length(q);
                        var g = (diag.x + diag.y + diag.z) / 3.0;
                        if reach_q > 1.0e-6 * blend {
                            g = length(pv_apply(diag, off, q)) / reach_q;
                        }
                        let wq = w / blend * PV_FIXED;
                        let base = PV_SUMS * ts_local(node);
                        atomicAdd(&pv_sum[base], i32(round(w * PV_FIXED)));
                        atomicAdd(&pv_sum[base + 1u], i32(round(wq * q.x)));
                        atomicAdd(&pv_sum[base + 2u], i32(round(wq * q.y)));
                        atomicAdd(&pv_sum[base + 3u], i32(round(wq * q.z)));
                        atomicAdd(&pv_sum[base + 4u], i32(round(wq * r)));
                        atomicAdd(&pv_sum[base + 5u], i32(round(w * g * PV_FIXED)));
                        atomicAdd(&pv_sum[base + 6u], i32(round(w * count)));
                        atomicAdd(&pv_sum[base + 7u], i32(round(w * moving.x)));
                        atomicAdd(&pv_sum[base + 8u], i32(round(w * moving.y)));
                        atomicAdd(&pv_sum[base + 9u], i32(round(w * moving.z)));
                    }
                }
            }
        }
    }
    workgroupBarrier();
    for (var l = lane; l < 512u; l = l + PV_THREADS) {
        let e = slot * 512u + l;
        // phi = (|q̄| - r̄) / ḡ in R units, from the weighted means over the
        // blobs within R; no blob within R is air.
        let base = PV_SUMS * l;
        let weight = f32(atomicLoad(&pv_sum[base]));
        var phi = band;
        var smoothing = 0.0;
        var moving = vec3<f32>(0.0);
        if weight >= PV_MIN_WEIGHT * PV_FIXED {
            let total = weight / PV_FIXED;
            let count = f32(atomicLoad(&pv_sum[base + 6u])) / PV_FIXED_COUNT / total;
            smoothing = clamp((count - params.fade_start) / (params.fade_end - params.fade_start), 0.0, 1.0);
            moving = vec3<f32>(
                f32(atomicLoad(&pv_sum[base + 7u])),
                f32(atomicLoad(&pv_sum[base + 8u])),
                f32(atomicLoad(&pv_sum[base + 9u])),
            ) / PV_FIXED_VELOCITY / total;
            let qbar = vec3<f32>(
                f32(atomicLoad(&pv_sum[base + 1u])),
                f32(atomicLoad(&pv_sum[base + 2u])),
                f32(atomicLoad(&pv_sum[base + 3u])),
            ) / weight;
            let rbar = f32(atomicLoad(&pv_sum[base + 4u])) / weight;
            let gbar = f32(atomicLoad(&pv_sum[base + 5u])) / weight;
            phi = min((length(qbar) - rbar) / max(gbar, 1.0e-6) * blend, band);
        }
        if solid[e] < 0.0 {
            phi = max(phi, -solid[e] - PV_WALL_REACH * h);
        }
        levelset[e] = phi;
        weights[e] = smoothing;
        velocity[3u * e] = moving.x;
        velocity[3u * e + 1u] = moving.y;
        velocity[3u * e + 2u] = moving.z;
    }
}
