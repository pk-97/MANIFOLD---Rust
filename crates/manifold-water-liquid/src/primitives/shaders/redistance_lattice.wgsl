// node.redistance_lattice (redistance_lattice.rs): the exact Euclidean
// distance, in metres, from each node to the piecewise-linear marching-cubes
// zero surface of `levelset`, capped at the band (band_cells cell sizes),
// signed by the input. A geometric distance rebuild, not a blur. Composed
// after the world-tile read side and the marching-cubes tables.
//
// One workgroup per pool slot. It loads the levelset over its tile and the
// RD_MAX_REACH + 1 nodes round it into workgroup memory once, marks the cells
// the zero surface crosses, and each node then searches every cell its band
// can reach, reading corners from workgroup memory and building triangles
// only in crossed cells. A window the surface crosses nowhere puts every node
// at the band, signed, without a search. A node in a tile the table does not
// hold reads the canonical element, far from the surface. The canonical
// element and a free slot hold the canonical level set.

struct Params {
    cell_size: f32,
    band_cells: f32,
    pool_tiles: u32,
    entries: u32,
}

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> levelset: array<f32>;
@group(0) @binding(2) var<storage, read> tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> out: array<f32>;

fn ts_pool_tiles() -> u32 { return u.pool_tiles; }
fn ts_table_entries() -> u32 { return u.entries; }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// The widest band in whole cells the workgroup's window holds
// (`MAX_BAND_CELLS` in redistance_lattice.rs).
const RD_MAX_REACH: i32 = 4;
// Window nodes per axis: the tile's 8, a node's search reach + 1 below and
// reach + 1 above (the far corner of its last cell).
const RD_NODES: i32 = 8 + 2 * (RD_MAX_REACH + 1);
const RD_NODES_ALL: u32 = 5832u;
// Window cells per axis, one fewer than its nodes.
const RD_CELLS: i32 = RD_NODES - 1;
const RD_CELLS_ALL: u32 = 4913u;
const RD_CROSS_WORDS: u32 = 154u;
const RD_THREADS: u32 = 256u;

var<workgroup> rd_nodes: array<f32, 5832>;
var<workgroup> rd_cross: array<atomic<u32>, 154>;
var<workgroup> rd_held: u32;
var<workgroup> rd_crossed: atomic<u32>;
var<workgroup> rd_any: u32;
// rd_cross once it is built, read without atomics.
var<workgroup> rd_crossings: array<u32, 154>;

fn rd_segment(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let v = b - a;
    let vv = dot(v, v);
    if vv == 0.0 { return dot(p-a, p-a); }
    let d = p - (a + clamp(dot(p-a, v)/vv, 0.0, 1.0)*v);
    return dot(d,d);
}

fn rd_triangle(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>, c: vec3<f32>) -> f32 {
    let ab = b-a;
    let ac = c-a;
    let n = cross(ab, ac);
    let nn = dot(n,n);
    var best = min(rd_segment(p,a,b), min(rd_segment(p,b,c), rd_segment(p,c,a)));
    if nn > 0.0 {
        let q = p-a;
        let u = dot(cross(q,ac), n)/nn;
        let v = dot(cross(ab,q), n)/nn;
        if u >= 0.0 && v >= 0.0 && u+v <= 1.0 {
            best = min(best, dot(q,n)*dot(q,n)/nn);
        }
    }
    return best;
}

fn rd_node_index(w: vec3<i32>) -> u32 {
    return u32(w.x + RD_NODES * (w.y + RD_NODES * w.z));
}

fn rd_cell_index(w: vec3<i32>) -> u32 {
    return u32(w.x + RD_CELLS * (w.y + RD_CELLS * w.z));
}

// The marching-cubes case of window cell w: bit c set when corner c is inside.
fn rd_case(w: vec3<i32>) -> u32 {
    var case_index = 0u;
    for (var c=0u; c<8u; c=c+1u) {
        if rd_nodes[rd_node_index(w + vec3<i32>(MC_CORNERS[c]))] < 0.0 {
            case_index = case_index | (1u << c);
        }
    }
    return case_index;
}

// The nearest-triangle distance² from the node at window node `at` to the
// crossed window cell `cell`, or `best` when it is no nearer. Positions are
// metres from the node, so the node is the origin.
fn rd_cell(at: vec3<i32>, cell: vec3<i32>, best: f32) -> f32 {
    let cell_size = u.cell_size;
    let box_low = vec3<f32>(cell - at) * cell_size;
    let box_high = vec3<f32>(cell - at + vec3<i32>(1)) * cell_size;
    let delta = max(max(box_low, -box_high), vec3<f32>(0.0));
    if dot(delta, delta) > best { return best; }
    let flat = rd_cell_index(cell);
    if (rd_crossings[flat >> 5u] & (1u << (flat & 31u))) == 0u { return best; }
    var corner: array<f32, 8>;
    var case_index = 0u;
    for (var c = 0u; c < 8u; c = c + 1u) {
        corner[c] = rd_nodes[rd_node_index(cell + vec3<i32>(MC_CORNERS[c]))];
        if corner[c] < 0.0 { case_index = case_index | (1u << c); }
    }
    let triangles = MC_TRIANGLE_COUNT[case_index];
    var vertices: array<vec3<f32>, 12>;
    // An edge crosses where the case's inside bits differ: one
    // classification per corner, so a corner of exactly zero can never be
    // inside for the case and outside for its edges (which would leave a
    // triangle vertex unset at the node).
    for (var e = 0u; e < 12u; e = e + 1u) {
        let ca = MC_EDGE_A[e];
        let cb = MC_EDGE_B[e];
        if ((case_index >> ca) & 1u) != ((case_index >> cb) & 1u) {
            let va = corner[ca];
            let d = va - corner[cb];
            let t = select(0.5, va / d, d != 0.0);
            let a = cell + vec3<i32>(MC_CORNERS[ca]) - at;
            let b = cell + vec3<i32>(MC_CORNERS[cb]) - at;
            vertices[e] = (vec3<f32>(a) + t * vec3<f32>(b - a)) * cell_size;
        }
    }
    let p = vec3<f32>(0.0);
    var nearest = best;
    for (var t = 0u; t < triangles; t = t + 1u) {
        nearest = min(nearest, rd_triangle(p, vertices[mc_edge(case_index, 3u * t)],
            vertices[mc_edge(case_index, 3u * t + 1u)], vertices[mc_edge(case_index, 3u * t + 2u)]));
    }
    return nearest;
}

// The distance from the node at window node `at` (tile-local `local`): the
// nearest marching-cubes triangle in the band's cells. The cells are
// searched in shells outward from the node, shell s the cells s cells away
// along their farthest axis, so every cell of a later shell is at least
// (s + 1) cells away: once the nearest found is that close, the rest cannot
// be nearer and the search stops.
fn rd_distance(local: vec3<i32>) -> f32 {
    let base = RD_MAX_REACH + 1;
    let at = local + vec3<i32>(base);
    let original = rd_nodes[rd_node_index(at)];
    let cell_size = u.cell_size;
    let band_m = u.band_cells * cell_size;
    let reach = i32(ceil(u.band_cells));
    var best = band_m * band_m;
    for (var s = 0; s <= reach; s = s + 1) {
        let low = at - vec3<i32>(s + 1);
        let high = at + vec3<i32>(s);
        for (var z = low.z; z <= high.z; z = z + 1) {
            for (var y = low.y; y <= high.y; y = y + 1) {
                // Off the shell's faces in y and z only its two x ends are on it.
                let face = z == low.z || z == high.z || y == low.y || y == high.y;
                let step = select(high.x - low.x, 1, face);
                for (var x = low.x; x <= high.x; x = x + step) {
                    best = rd_cell(at, vec3<i32>(x, y, z), best);
                }
            }
        }
        let next = f32(s + 1) * cell_size;
        if best <= next * next { break; }
    }
    return select(sqrt(best), -sqrt(best), original < 0.0);
}

@compute @workgroup_size(256)
fn cs_main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    // FIELD_FAR_CELLS (scene_object) cells: the level set where no tile is stored.
    let far = 3.0 * u.cell_size;
    if lane == 0u {
        atomicStore(&rd_crossed, 0u);
        rd_held = u32(slot < u.pool_tiles && ts_slot_entry(slot) != WT_NONE);
        if slot == u.pool_tiles {
            out[ts_canon()] = far;
        }
    }
    if lane < RD_CROSS_WORDS {
        atomicStore(&rd_cross[lane], 0u);
    }
    let held = workgroupUniformLoad(&rd_held) != 0u;
    if !held {
        if slot < u.pool_tiles {
            out[slot * 512u + lane] = far;
            out[slot * 512u + lane + RD_THREADS] = far;
        }
        return;
    }
    ts_home(slot * 512u);
    let origin = ts_home_tile * 8 - vec3<i32>(RD_MAX_REACH + 1);
    for (var i = lane; i < RD_NODES_ALL; i = i + RD_THREADS) {
        let w = vec3<i32>(i32(i) % RD_NODES, (i32(i) / RD_NODES) % RD_NODES, i32(i) / (RD_NODES * RD_NODES));
        rd_nodes[i] = levelset[ts_cell_at(origin + w)];
    }
    workgroupBarrier();
    for (var i = lane; i < RD_CELLS_ALL; i = i + RD_THREADS) {
        let w = vec3<i32>(i32(i) % RD_CELLS, (i32(i) / RD_CELLS) % RD_CELLS, i32(i) / (RD_CELLS * RD_CELLS));
        if MC_TRIANGLE_COUNT[rd_case(w)] != 0u {
            atomicOr(&rd_cross[i >> 5u], 1u << (i & 31u));
            atomicStore(&rd_crossed, 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u {
        rd_any = atomicLoad(&rd_crossed);
    }
    if lane < RD_CROSS_WORDS {
        rd_crossings[lane] = atomicLoad(&rd_cross[lane]);
    }
    if workgroupUniformLoad(&rd_any) == 0u {
        // No triangle anywhere in reach: the search would end on the band.
        let band_m = u.band_cells * u.cell_size;
        let band = sqrt(band_m * band_m);
        let base = vec3<i32>(RD_MAX_REACH + 1);
        for (var n = lane; n < 512u; n = n + RD_THREADS) {
            let original = rd_nodes[rd_node_index(ts_local_coord(n) + base)];
            out[slot * 512u + n] = select(band, -band, original < 0.0);
        }
        return;
    }
    for (var n = lane; n < 512u; n = n + RD_THREADS) {
        out[slot * 512u + n] = rd_distance(ts_local_coord(n));
    }
}
