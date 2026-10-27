// node.tile_surface_mesh (tile_surface_mesh.rs): the marching-cubes triangle
// list of a pooled level set's zero surface, built tile by tile. Composed
// after the world-tile read side and the marching-cubes tables
// (marching_cubes_common.wgsl: bit c of a case is set when corner c is
// inside, the level set below zero).
//
// Passes, a barrier between each: sm_reset, sm_tiles, sm_finish, sm_tail.
// sm_tiles runs one workgroup per pool slot. It loads the nodes round its
// tile into workgroup memory (one below, two above on every axis: the corners
// of the tile's 8³ cells, the cells round their edges and the central
// differences at them), counts its cells' triangles, takes its run of the
// vertex buffer with one atomic and writes them, each vertex relaxed once
// toward its neighbours (sm_relaxed). Node p sits at (p - 1/2)·cell in the object's space; a node in
// a tile the table does not hold reads the canonical element. An edge's
// vertex comes from its two nodes in the same order whichever tile owns the
// cell, so neighbouring tiles' triangles meet exactly. A free slot writes
// nothing.
//
// counts: word 0 the live triangles (what draws), word 1 the triangles the
// surface asked for, word 2 last frame's live triangles. Past the buffer's
// capacity triangles are dropped and word 1 still counts them, so the CPU
// grows the buffer from it. Every vertex past the live triangles is zero.

struct Params {
    cell_size: f32,
    pool_tiles: u32,
    entries: u32,
    // Triangles the vertex buffer holds.
    capacity: u32,
}

// `MeshVertex` (manifold_node_engine::mesh), 80 bytes.
struct MeshVertex {
    position: vec3<f32>,
    pad0: f32,
    normal: vec3<f32>,
    pad1: f32,
    uv: vec2<f32>,
    uv1: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> levelset: array<f32>;
@group(0) @binding(2) var<storage, read> tiles: array<u32>;
@group(0) @binding(3) var<storage, read_write> vertices: array<MeshVertex>;
@group(0) @binding(4) var<storage, read_write> counts: array<atomic<u32>>;

fn ts_pool_tiles() -> u32 { return u.pool_tiles; }
fn ts_table_entries() -> u32 { return u.entries; }
fn ts_table_word(i: u32) -> u32 { return tiles[i]; }

// Window nodes per axis: the tile's 8, one below and two above.
const SM_SIDE: i32 = 11;
const SM_NODES: u32 = 1331u;
const SM_THREADS: u32 = 256u;

var<workgroup> sm_phi: array<f32, 1331>;
var<workgroup> sm_held: u32;
var<workgroup> sm_count: atomic<u32>;
var<workgroup> sm_base: u32;

// The level set at window node w (tile-local node w - 1).
fn sm_at(w: vec3<i32>) -> f32 {
    return sm_phi[u32(w.x + SM_SIDE * (w.y + SM_SIDE * w.z))];
}

// Central-difference gradient at window node w, in level-set units per
// metre. Points outward: the level set grows outside.
fn sm_gradient(w: vec3<i32>) -> vec3<f32> {
    let x = vec3<i32>(1, 0, 0);
    let y = vec3<i32>(0, 1, 0);
    let z = vec3<i32>(0, 0, 1);
    return vec3<f32>(
        sm_at(w + x) - sm_at(w - x),
        sm_at(w + y) - sm_at(w - y),
        sm_at(w + z) - sm_at(w - z),
    ) / (2.0 * u.cell_size);
}

// The case of tile-local cell c.
fn sm_case(c: vec3<i32>) -> u32 {
    var case_index = 0u;
    for (var k = 0u; k < 8u; k = k + 1u) {
        if sm_at(c + vec3<i32>(1) + vec3<i32>(MC_CORNERS[k])) < 0.0 {
            case_index = case_index | (1u << k);
        }
    }
    return case_index;
}

// Edge `edge` of tile-local cell c: its two tile-local nodes, the lower first
// (they differ along one axis), and where the level set crosses it, 0 at lo.
struct SmEdge {
    lo: vec3<i32>,
    hi: vec3<i32>,
    mu: f32,
}

fn sm_edge(c: vec3<i32>, edge: u32) -> SmEdge {
    var a = c + vec3<i32>(MC_CORNERS[MC_EDGE_A[edge]]);
    var b = c + vec3<i32>(MC_CORNERS[MC_EDGE_B[edge]]);
    if any(b < a) {
        let swap = a;
        a = b;
        b = swap;
    }
    let phi_a = sm_at(a + vec3<i32>(1));
    let phi_b = sm_at(b + vec3<i32>(1));
    return SmEdge(a, b, clamp(-phi_a / (phi_b - phi_a), 1e-6, 1.0 - 1e-6));
}

// Where edge e's vertex sits in the object's space, the tile's first node at
// world node `origin`.
fn sm_point(e: SmEdge, origin: vec3<i32>) -> vec3<f32> {
    let node = vec3<f32>(origin + e.lo) + e.mu * vec3<f32>(e.hi - e.lo);
    return (node - vec3<f32>(0.5)) * u.cell_size;
}

// How far each vertex moves toward the mean of its neighbours, in one Jacobi
// pass of FLIP Fluids' umbrella smoothing (trianglemesh.cpp smooth): about
// the two passes at 0.5 the water's mesh took before. It rounds a drop of a
// cell or two and thins a sheet without deleting either, which smoothing the
// level set would.
const SM_RELAX: f32 = 1.0;

// Edge e's vertex moved SM_RELAX of the way to the mean of the vertices it
// shares a triangle with. They lie in the four cells round its edge, walked in
// an order fixed by the edge, so every copy of the vertex (in any cell or
// tile) sums the same values in the same order and lands on the same bits:
// the mesh stays closed. Each neighbour is met twice, from the triangles on
// either side of the edge they share, so the mean over meetings is the mean
// over neighbours. The cells reach one node below the edge, inside the window.
fn sm_relaxed(e: SmEdge, origin: vec3<i32>) -> vec3<f32> {
    let here = sm_point(e, origin);
    let along = e.hi - e.lo;
    let across_a = along.yzx;
    let across_b = along.zxy;
    var sum = vec3<f32>(0.0);
    var met = 0u;
    for (var k = 0; k < 4; k = k + 1) {
        let n = e.lo - (k & 1) * across_a - ((k >> 1) & 1) * across_b;
        let case_index = sm_case(n);
        for (var t = 0u; t < MC_TRIANGLE_COUNT[case_index]; t = t + 1u) {
            var corners: array<SmEdge, 3>;
            var hit = 3u;
            for (var corner = 0u; corner < 3u; corner = corner + 1u) {
                corners[corner] = sm_edge(n, mc_edge(case_index, 3u * t + corner));
                if all(corners[corner].lo == e.lo) && all(corners[corner].hi == e.hi) {
                    hit = corner;
                }
            }
            if hit == 3u {
                continue;
            }
            for (var corner = 0u; corner < 3u; corner = corner + 1u) {
                if corner != hit {
                    sum = sum + sm_point(corners[corner], origin);
                    met = met + 1u;
                }
            }
        }
    }
    if met == 0u {
        return here;
    }
    return here + SM_RELAX * (sum / f32(met) - here);
}

// The vertex on edge `edge` of tile-local cell c, relaxed, its normal the
// level set's gradient where the edge crosses it.
fn sm_vertex(c: vec3<i32>, edge: u32, origin: vec3<i32>) -> MeshVertex {
    let e = sm_edge(c, edge);
    let position = sm_relaxed(e, origin);
    var normal = mix(sm_gradient(e.lo + vec3<i32>(1)), sm_gradient(e.hi + vec3<i32>(1)), e.mu);
    let length_squared = dot(normal, normal);
    if length_squared > 0.0 {
        normal = normal * inverseSqrt(length_squared);
    } else {
        normal = vec3<f32>(0.0, 1.0, 0.0);
    }
    return MeshVertex(
        position, 0.0, normal, 0.0, position.xz, vec2<f32>(0.0),
        vec4<f32>(1.0, 0.0, 0.0, 1.0), vec4<f32>(1.0),
    );
}

// One thread: last frame's live count kept, this frame's asked count cleared.
@compute @workgroup_size(1)
fn sm_reset() {
    atomicStore(&counts[2], atomicLoad(&counts[0]));
    atomicStore(&counts[1], 0u);
}

@compute @workgroup_size(256)
fn sm_tiles(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    if lane == 0u {
        sm_held = u32(slot < u.pool_tiles && ts_slot_entry(slot) != WT_NONE);
        atomicStore(&sm_count, 0u);
    }
    if workgroupUniformLoad(&sm_held) == 0u {
        return;
    }
    ts_home(slot * 512u);
    let origin = ts_home_tile * 8;
    for (var i = lane; i < SM_NODES; i = i + SM_THREADS) {
        let n = i32(i);
        let w = vec3<i32>(n % SM_SIDE, (n / SM_SIDE) % SM_SIDE, n / (SM_SIDE * SM_SIDE));
        sm_phi[i] = levelset[ts_cell_at(origin - vec3<i32>(1) + w)];
    }
    workgroupBarrier();
    // Two cells a thread: each counts its triangles into the tile's run.
    var cases: array<u32, 2>;
    var firsts: array<u32, 2>;
    for (var k = 0u; k < 2u; k = k + 1u) {
        let c = ts_local_coord(lane + k * SM_THREADS);
        cases[k] = sm_case(c);
        let triangles = MC_TRIANGLE_COUNT[cases[k]];
        if triangles != 0u {
            firsts[k] = atomicAdd(&sm_count, triangles);
        }
    }
    workgroupBarrier();
    if lane == 0u {
        let total = atomicLoad(&sm_count);
        sm_base = 0u;
        if total != 0u {
            sm_base = atomicAdd(&counts[1], total);
        }
    }
    let base = workgroupUniformLoad(&sm_base);
    for (var k = 0u; k < 2u; k = k + 1u) {
        let triangles = MC_TRIANGLE_COUNT[cases[k]];
        let c = ts_local_coord(lane + k * SM_THREADS);
        for (var t = 0u; t < triangles; t = t + 1u) {
            let triangle = base + firsts[k] + t;
            if triangle >= u.capacity {
                break;
            }
            for (var corner = 0u; corner < 3u; corner = corner + 1u) {
                vertices[3u * triangle + corner] = sm_vertex(c, mc_edge(cases[k], 3u * t + corner), origin);
            }
        }
    }
}

// One thread: the live triangles, the asked ones within the capacity.
@compute @workgroup_size(1)
fn sm_finish() {
    atomicStore(&counts[0], min(atomicLoad(&counts[1]), u.capacity));
}

// A thread a vertex up to the capacity: zero what last frame drew past this
// frame's live triangles.
@compute @workgroup_size(256)
fn sm_tail(@builtin(global_invocation_id) gid: vec3<u32>) {
    let v = gid.x;
    let last = min(atomicLoad(&counts[2]), u.capacity);
    if v >= 3u * last || v < 3u * atomicLoad(&counts[0]) {
        return;
    }
    vertices[v] = MeshVertex(
        vec3<f32>(0.0), 0.0, vec3<f32>(0.0), 0.0, vec2<f32>(0.0), vec2<f32>(0.0),
        vec4<f32>(0.0), vec4<f32>(0.0),
    );
}
