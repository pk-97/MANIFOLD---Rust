// Surface weights: per node of the display's world tiles, how much smoothing
// the surface gets (1 in bulk water, 0 on thin sheets and drops) and the
// water's velocity. A pure function of this frame's particles and the
// splatted field.
//
// Two passes, in command order. cs_counts gives every particle the number of
// live neighbours within count_reach. cs_weights gathers, for every node
// inside the field's band, the poly6-weighted mean count and velocity of
// the particles within carry_reach, and fades the count into a weight.
//
// The particles are sorted into the display cells (`cell_ranges`, one range per
// pool element of the world tiles in `buf_tiles`), so both passes read only
// the cells a ball of their reach can touch. Element p of `reference`,
// `weights` and `velocity` (three floats, xyz at 3p..3p+2) is display node p,
// at (p - 1/2) cells. A free slot and the canonical element write 0.

struct Params {
    cell: f32,
    count_reach: f32,
    carry_reach: f32,
    band: f32,
    fade_start: f32,
    fade_end: f32,
    count_cells: f32,
    carry_cells: f32,
    pool_tiles: u32,
    particle_capacity: u32,
    element_count: u32,
    pad: u32,
}

struct Particle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
}

struct CellRange {
    start: u32,
    count: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> cell_ranges: array<CellRange>;
@group(0) @binding(3) var<storage, read> reference: array<f32>;
@group(0) @binding(4) var<storage, read_write> counts: array<f32>;
@group(0) @binding(5) var<storage, read_write> weights: array<f32>;
@group(0) @binding(6) var<storage, read_write> velocity: array<f32>;
@group(0) @binding(7) var<storage, read> buf_tiles: array<u32>;

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_entries() -> u32 { let n = 4u * max(ww_pool_tiles, 1u); return 1u << (32u - countLeadingZeros(n - 1u)); }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// The most cells a search window spans on an axis: a ball of 2 cells reaches
// 4 or 5. A window of any other size is a non-finite position's, and is cut
// short rather than walked.
const MAX_SPAN: i32 = 8;

// A window of cells: `lo` the first and `span` how many on each axis.
struct CellWindow {
    lo: vec3<i32>,
    span: vec3<i32>,
}

fn window(lo: vec3<i32>, hi: vec3<i32>) -> CellWindow {
    var w: CellWindow;
    w.lo = lo;
    w.span = clamp(hi - lo + vec3<i32>(1), vec3<i32>(0), vec3<i32>(MAX_SPAN));
    return w;
}

// The cells whose particles can lie within `reach_cells` of node coordinate
// q = x / h + 1/2 (a particle's cell is floor(q)). The slack covers rounding in
// the sort's own product, so a window never falls one cell short; it only
// widens the search, the distance tests stay exact.
fn cell_window(q: vec3<f32>, reach_cells: f32) -> CellWindow {
    let slack = vec3<f32>(1.0e-6) * (abs(q) + vec3<f32>(reach_cells + 1.0));
    return window(
        vec3<i32>(floor(q - vec3<f32>(reach_cells) - slack)),
        vec3<i32>(floor(q + vec3<f32>(reach_cells) + slack)),
    );
}

// Whether pool element e is a cell of a held tile (not a free slot or canonical).
fn held(e: u32) -> bool {
    return e < ts_canon() && ts_slot_entry(e >> 9u) != WT_NONE;
}

@compute @workgroup_size(64)
fn cs_counts(@builtin(global_invocation_id) gid: vec3<u32>) {
    let k = gid.x;
    if k >= params.particle_capacity {
        return;
    }
    ww_pool_tiles = params.pool_tiles;
    let self_particle = particles[k];
    if !(self_particle.position_radius.w > 0.0) {
        counts[k] = 0.0;
        return;
    }
    let p = self_particle.position_radius.xyz;
    let q = p / params.cell + vec3<f32>(0.5);
    ts_home(ts_cell_at(vec3<i32>(floor(q))));
    let reach2 = params.count_reach * params.count_reach;
    let search = cell_window(q, params.count_cells);
    let canon = ts_canon();
    var found = 0u;
    for (var dz = 0; dz < search.span.z; dz = dz + 1) {
        for (var dy = 0; dy < search.span.y; dy = dy + 1) {
            for (var dx = 0; dx < search.span.x; dx = dx + 1) {
                let e = ts_cell_at(search.lo + vec3<i32>(dx, dy, dz));
                if e >= canon {
                    continue;
                }
                let range = cell_ranges[e];
                for (var j = range.start; j < range.start + range.count; j = j + 1u) {
                    if j == k {
                        continue;
                    }
                    let other = particles[j];
                    if !(other.position_radius.w > 0.0) {
                        continue;
                    }
                    let d = other.position_radius.xyz - p;
                    if dot(d, d) < reach2 {
                        found = found + 1u;
                    }
                }
            }
        }
    }
    counts[k] = f32(found);
}

@compute @workgroup_size(64)
fn cs_weights(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = gid.x;
    if n >= params.element_count {
        return;
    }
    ww_pool_tiles = params.pool_tiles;
    if !held(n) || abs(reference[n]) >= params.band {
        weights[n] = 0.0;
        velocity[3u * n] = 0.0;
        velocity[3u * n + 1u] = 0.0;
        velocity[3u * n + 2u] = 0.0;
        return;
    }
    ts_home(n);
    let node = ts_cell_coord(n);
    let x_node = (vec3<f32>(node) - vec3<f32>(0.5)) * params.cell;
    let carry2 = params.carry_reach * params.carry_reach;
    // A node sits on a cell corner, so the particles within carry_cells of it
    // fall in the cells from node - ceil(carry_cells) to node + ceil(carry_cells) - 1;
    // the kernel is 0 at the edge, so a particle the sort places just outside
    // adds nothing.
    let whole = vec3<i32>(i32(ceil(params.carry_cells)));
    let search = window(node - whole, node + whole - vec3<i32>(1));
    let canon = ts_canon();
    var sw = 0.0;
    var sn = 0.0;
    var sv = vec3<f32>(0.0);
    for (var dz = 0; dz < search.span.z; dz = dz + 1) {
        for (var dy = 0; dy < search.span.y; dy = dy + 1) {
            for (var dx = 0; dx < search.span.x; dx = dx + 1) {
                let e = ts_cell_at(search.lo + vec3<i32>(dx, dy, dz));
                if e >= canon {
                    continue;
                }
                let range = cell_ranges[e];
                for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                    let particle = particles[k];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let d = particle.position_radius.xyz - x_node;
                    let r2 = dot(d, d) / carry2;
                    if !(r2 < 1.0) {
                        continue;
                    }
                    let t = 1.0 - r2;
                    let w = t * t * t;
                    sw = sw + w;
                    sn = sn + w * counts[k];
                    sv = sv + w * particle.velocity;
                }
            }
        }
    }
    var nbar = 0.0;
    var v = vec3<f32>(0.0);
    if sw > 0.0 {
        nbar = sn / sw;
        v = sv / sw;
    }
    weights[n] = clamp((nbar - params.fade_start) / (params.fade_end - params.fade_start), 0.0, 1.0);
    velocity[3u * n] = v.x;
    velocity[3u * n + 1u] = v.y;
    velocity[3u * n + 2u] = v.z;
}
