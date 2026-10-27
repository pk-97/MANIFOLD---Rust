// node.matter_to_grid — MLS-MPM particle-to-grid. Hand WGSL under exclusion 1 of ADDING_PRIMITIVES.md
// (workgroup memory and barriers). Each live point adds to the 27 grid nodes
// of its quadratic B-spline stencil:
//   mass     += w · m_p
//   momentum += w · (m_p·v_p + (m_p·C_p − dt·V0·(4/dx²)·τ_p·I)·d_i)
// with the water Kirchhoff pressure τ = λ·J·(J − 1) (× Cohesion for J ≥ 1).
// Node p sits at p·dx and lives in the world tiles' pooled arrays (the
// including source prepends the table's read side): word w of node p is
// accum[ts_cell_at(p)·4 + w], the canonical element where no tile is held.
// Words per node: momentum x, y, z, mass; signed fixed point, mass in
// m_unit = 1000·dx³/8 kg at 2^16, momentum in m_unit·U at 2^27 (U the
// domain's power-of-two momentum unit, at or above dx/dt), each
// contribution floor(x + u) with u hashed from the point id and the word's
// node key (D5), so the sums do not depend on where the tiles sit.
//
// One workgroup per 4³ block of stencil base nodes (the sorter's key,
// `ranges[block]` over `order`): block 8·slot + b is octant b of slot's tile,
// block 8P the points in tiles the table does not hold. A held block
// accumulates into a 6³-node workgroup tile, flushed once per nonzero word;
// a point that has left its block since the sort adds globally. Integer sums
// make both routes bit-identical.
//
// A point whose scatter inputs are not finite is skipped; node.matter_stats
// counts the same points, so D14 halts the publish.

struct MatterPoint {
    position: vec3<f32>,
    id: u32,
    velocity: vec3<f32>,
    volume_ratio: f32,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
}

struct CellRange {
    start: u32,
    count: u32,
}

struct P2gParams {
    cell_size: f32,
    active_count: u32,
    step_dt: f32,
    lambda: f32,
    cohesion: f32,
    density: f32,
    tick_index: u32,
    substep_in_tick: u32,
    momentum_unit: f32,
    pool_tiles: u32,
    table_entries: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: P2gParams;
@group(0) @binding(1) var<storage, read> points: array<MatterPoint>;
@group(0) @binding(2) var<storage, read_write> accum: array<atomic<i32>>;
@group(0) @binding(3) var<storage, read> order: array<u32>;
@group(0) @binding(4) var<storage, read> ranges: array<CellRange>;
@group(0) @binding(5) var<storage, read> ts_table: array<u32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.table_entries; }
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }

const GROUP: u32 = 256u;
const TILE: i32 = 6;
const TILE_WORDS: u32 = 864u; // 6³ nodes × 4 words
const NO_RANK: u32 = 0xffffffffu;

var<workgroup> tile: array<atomic<i32>, 864>;
var<workgroup> block_start: u32;
var<workgroup> block_count: u32;

fn finite3(v: vec3<f32>) -> bool {
    let e = vec3<u32>(bitcast<u32>(v.x), bitcast<u32>(v.y), bitcast<u32>(v.z)) & vec3<u32>(0x7f800000u);
    return all(e != vec3<u32>(0x7f800000u));
}

fn hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// The rounding key of word w of a node: a function of the node's coordinates
// alone (crate::matter::node_word_key), so a node rounds the same wherever
// its tile sits.
fn node_word(node: vec3<i32>, w: u32) -> u32 {
    let u = bitcast<vec3<u32>>(node);
    return ((u.x * 73856093u) ^ (u.y * 19349663u) ^ (u.z * 83492791u)) * 4u + w;
}

// floor(x + u) with the carry in integers: an f32 sum x + u would round first
// and bias every word upward by about |x|·2^-24.
fn encode(x: f32, key: u32, slot: u32) -> i32 {
    let whole = floor(x);
    let fraction = u32((x - whole) * 16777216.0);
    let carry = (fraction + (hash(key ^ slot) >> 8u)) >> 24u;
    return i32(whole) + i32(carry);
}

// Adds one point to the grid: to the workgroup tile when its whole stencil
// lies in the tile at `tile_origin` (and `use_tile`), otherwise to the global
// accumulator.
fn scatter(pt: MatterPoint, use_tile: bool, tile_origin: vec3<i32>) {
    if pt.id == 0u || !finite3(pt.position) {
        return;
    }
    let cell_size = params.cell_size;
    let inv_dx = 1.0 / cell_size;
    let q = pt.position * inv_dx;
    let base = floor(q - vec3<f32>(0.5));
    let base_i = vec3<i32>(base);
    let f = q - base;
    let w0 = 0.5 * (vec3<f32>(1.5) - f) * (vec3<f32>(1.5) - f);
    let w1 = vec3<f32>(0.75) - (f - vec3<f32>(1.0)) * (f - vec3<f32>(1.0));
    let w2 = 0.5 * (f - vec3<f32>(0.5)) * (f - vec3<f32>(0.5));

    let j = pt.volume_ratio;
    let v0 = pt.affine_y.w;
    let mass = v0 * params.density;
    var tau = params.lambda * j * (j - 1.0);
    if j >= 1.0 {
        tau = tau * params.cohesion;
    }
    if !(finite3(pt.velocity) && finite3(pt.affine_x.xyz) && finite3(pt.affine_y.xyz)
        && finite3(pt.affine_z.xyz) && finite3(vec3<f32>(j, v0, tau))) {
        return;
    }
    let stress = params.step_dt * v0 * 4.0 * inv_dx * inv_dx * tau;
    // Affine rows: m·C − stress·I.
    let a0 = mass * pt.affine_x.xyz - vec3<f32>(stress, 0.0, 0.0);
    let a1 = mass * pt.affine_y.xyz - vec3<f32>(0.0, stress, 0.0);
    let a2 = mass * pt.affine_z.xyz - vec3<f32>(0.0, 0.0, stress);
    let mv = mass * pt.velocity;

    // Both scales are powers of two times one reciprocal, so their ratio is
    // exactly 2^11 / U and the grid update's decode inverts it.
    let inv_mass_unit = 1.0 / (125.0 * cell_size * cell_size * cell_size);
    let to_mass = 65536.0 * inv_mass_unit;
    let to_momentum = 134217728.0 / params.momentum_unit * inv_mass_unit;
    let key = hash(pt.id ^ hash(params.tick_index * 4096u + params.substep_in_tick));
    ts_home(ts_cell_at(base_i));
    let local_base = base_i - tile_origin;
    let in_tile = use_tile && all(local_base >= vec3<i32>(0)) && all(local_base + vec3<i32>(2) < vec3<i32>(TILE));

    for (var a = 0; a < 3; a = a + 1) {
        var wx = w0.x;
        if a == 1 { wx = w1.x; } else if a == 2 { wx = w2.x; }
        for (var b = 0; b < 3; b = b + 1) {
            var wy = w0.y;
            if b == 1 { wy = w1.y; } else if b == 2 { wy = w2.y; }
            for (var c = 0; c < 3; c = c + 1) {
                var wz = w0.z;
                if c == 1 { wz = w1.z; } else if c == 2 { wz = w2.z; }
                let weight = wx * wy * wz;
                let d = (vec3<f32>(f32(a), f32(b), f32(c)) - f) * cell_size;
                let momentum = weight * (mv + vec3<f32>(dot(a0, d), dot(a1, d), dot(a2, d)));
                let node = base_i + vec3<i32>(a, b, c);
                let words = vec4<i32>(
                    encode(momentum.x * to_momentum, key, node_word(node, 0u)),
                    encode(momentum.y * to_momentum, key, node_word(node, 1u)),
                    encode(momentum.z * to_momentum, key, node_word(node, 2u)),
                    encode(weight * mass * to_mass, key, node_word(node, 3u)),
                );
                if in_tile {
                    let l = local_base + vec3<i32>(a, b, c);
                    let t = u32((l.z * TILE + l.y) * TILE + l.x) * 4u;
                    atomicAdd(&tile[t], words.x);
                    atomicAdd(&tile[t + 1u], words.y);
                    atomicAdd(&tile[t + 2u], words.z);
                    atomicAdd(&tile[t + 3u], words.w);
                } else {
                    let slot = ts_cell_at(node) * 4u;
                    atomicAdd(&accum[slot], words.x);
                    atomicAdd(&accum[slot + 1u], words.y);
                    atomicAdd(&accum[slot + 2u], words.z);
                    atomicAdd(&accum[slot + 3u], words.w);
                }
            }
        }
    }
}

// Workgroup w is block w, over the points `order`/`ranges` put there.
@compute @workgroup_size(256)
fn scatter_main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let block = wg.x;
    if lid == 0u {
        let range = ranges[block];
        block_start = range.start;
        block_count = range.count;
    }
    let count = workgroupUniformLoad(&block_count);
    if count == 0u {
        return;
    }
    let begin = workgroupUniformLoad(&block_start);
    let end = begin + count;

    var use_tile = false;
    var tile_origin = vec3<i32>(0);
    var home_slot = WT_NONE;
    if block < 8u * params.pool_tiles {
        let slot = block >> 3u;
        let b = block & 7u;
        if ts_slot_entry(slot) != WT_NONE {
            use_tile = true;
            home_slot = slot;
            tile_origin = ts_slot_tile(slot) * 8 + vec3<i32>(i32(b & 1u), i32((b >> 1u) & 1u), i32(b >> 2u)) * 4;
        }
    }
    if use_tile {
        for (var i = lid; i < TILE_WORDS; i = i + GROUP) {
            atomicStore(&tile[i], 0);
        }
    }
    workgroupBarrier();
    for (var s = begin + lid; s < end; s = s + GROUP) {
        let index = order[s];
        if index != NO_RANK && index < params.active_count {
            scatter(points[index], use_tile, tile_origin);
        }
    }
    workgroupBarrier();
    if use_tile {
        ts_home(home_slot * 512u);
        for (var i = lid; i < TILE_WORDS; i = i + GROUP) {
            let value = atomicLoad(&tile[i]);
            if value != 0 {
                let local = i / 4u;
                let l = vec3<i32>(i32(local % 6u), i32((local / 6u) % 6u), i32(local / 36u));
                atomicAdd(&accum[ts_cell_at(tile_origin + l) * 4u + (i % 4u)], value);
            }
        }
    }
}
