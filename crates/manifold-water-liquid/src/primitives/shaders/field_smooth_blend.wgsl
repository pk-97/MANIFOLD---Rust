// node.field_smooth_blend — one step of the water field's smoothing over the
// nodes of a world-tile pool (`buf_tiles`, `pool_tiles` tiles), one workgroup
// per slot and one more for the canonical element. Element p is node p.
//
// The step moves each node toward the mean of `prior` along the surface
// through it: a ring of six points round the node, square to the normal,
// taken with the node itself. Every weight is positive, so the step can only
// flatten: it never makes a bump deeper than the bumps round it, and a flat
// surface or a straight distance field stays put. It holds the result within
// a leash of `reference`:
//   out = prior                                         where |reference| >= band
//   out = clamp(prior + k · (along − prior), reference ± reach)   elsewhere
// with along = prior / 4 + (the ring's six trilinear samples) / 8,
// k = mix(1/2, 1, w) and reach = mix(1, 2, w) · leash by the node's weight w
// (thin and lone water moves half as far), band = band_cells and leash =
// smoothness · FIELD_LEASH_CELLS, both in simulation cells, so every Surface
// Detail smooths the same water. The ring's radius gives (1 + s²) / 4 node²
// of each second derivative along the surface at s display nodes per
// simulation cell (1/4 at stride 1), so the smoothing reaches as far in
// metres at every Surface Detail. The normal comes from central differences
// a simulation cell apart. With keep_sheets (water), in the middle of a sheet
// or of a gap, where the gradient is under half and points along the sheet,
// it is the direction the field bends most, so the ring lies along the sheet
// instead of across it, and a node inside the water with water on both sides
// of it along the normal is a sheet's middle: it rises at most half way to
// the surface, so a step never opens a sheet. Without it (Matter), a thin
// bump of a few stray particles melts back into the surface instead of
// standing as a fin.
//
// The workgroup stages its tile and `halo` nodes round it (stride + 1)
// through its 27 neighbour tiles, then reads each band node's normal and
// ring from that staged copy. A node in a tile the table does not hold reads
// the canonical element of `prior`. A free slot and the canonical element
// hold the far value.

struct Params {
    cell_size: f32,
    smoothness: f32,
    band_cells: f32,
    pool_tiles: u32,
    entries: u32,
    // Display nodes per simulation cell along an axis, 1 to 3.
    stride: u32,
    sim_cell_size: f32,
    // 1 keeps thin sheets whole (water), 0 lets them melt (Matter).
    keep_sheets: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> prior: array<f32>;
@group(0) @binding(2) var<storage, read> weights: array<f32>;
@group(0) @binding(3) var<storage, read> reference: array<f32>;
@group(0) @binding(4) var<storage, read> buf_tiles: array<u32>;
@group(0) @binding(5) var<storage, read_write> field: array<f32>;

fn ts_pool_tiles() -> u32 { return params.pool_tiles; }
fn ts_table_entries() -> u32 { return params.entries; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

const FS_THREADS: u32 = 256u;
// field_build::FIELD_LEASH_CELLS.
const FS_LEASH_CELLS: f32 = 0.5;
// FIELD_FAR_CELLS (scene_object), the level set where no tile is held.
const FS_FAR_CELLS: f32 = 3.0;
var<workgroup> fs_held: u32;
// Whether any node of the tile lies in the band: a tile with none keeps
// `prior` and skips the staging and the passes.
var<workgroup> fs_band_any: atomic<u32>;
var<workgroup> fs_in_band: u32;
// The slots of the 27 tiles round this one, by neighbour index.
var<workgroup> fs_near: array<u32, 27>;
// `prior` over the staged region (side 8 + 2 · halo). The host sizes it for
// the stride its pipeline runs (16³ at stride 3), so a finer stride holds no
// workgroup memory it does not use.
const FS_A_LEN: u32 = 4096u;
var<workgroup> fs_a: array<f32, FS_A_LEN>;

// The staged region's halo and side.
fn fs_halo() -> u32 { return params.stride + 1u; }
fn fs_side() -> u32 { return 8u + 2u * fs_halo(); }

// The ring's radius in nodes: the node at 1/4 and six points at 1/8 each,
// radius d, add 3 d² / 16 node² of each second derivative along the
// surface, set to (1 + s²) / 4 node² (1/4 at stride 1). A sample reaches
// floor(d) + 1 nodes, the halo.
fn fs_ring_radius() -> f32 {
    let s = f32(params.stride);
    let lap = select((1.0 + s * s) / 4.0, 0.25, params.stride == 1u);
    return sqrt(16.0 * lap / 3.0);
}

// The gradient below which a node takes the bend for its normal.
const FS_WEAK_GRADIENT: f32 = 0.5;

// The ring's directions: six at 60 degrees.
const FS_RING = array<vec2<f32>, 6>(
    vec2<f32>(1.0, 0.0),
    vec2<f32>(0.5, 0.8660254),
    vec2<f32>(-0.5, 0.8660254),
    vec2<f32>(-1.0, 0.0),
    vec2<f32>(-0.5, -0.8660254),
    vec2<f32>(0.5, -0.8660254),
);

// The slot of the tile region node r lies in, WT_NONE when it is not held.
fn fs_slot(r: vec3<u32>) -> u32 {
    let halo = fs_halo();
    let d = select(vec3<i32>(0), vec3<i32>(-1), r < vec3<u32>(halo))
        + select(vec3<i32>(0), vec3<i32>(1), r >= vec3<u32>(halo + 8u));
    return fs_near[ts_neighbour_index(d)];
}

// `prior` at world node n from the staged region, `low` the region's first
// world node. A coordinate past the region is held
// at its edge: only a trilinear corner of weight 0 reaches past it.
fn fs_staged(low: vec3<i32>, n: vec3<i32>) -> f32 {
    let side = i32(fs_side());
    let r = vec3<u32>(clamp(n - low, vec3<i32>(0), vec3<i32>(side - 1)));
    return fs_a[r.x + u32(side) * (r.y + u32(side) * r.z)];
}

// The trilinear of the staged `prior` at q (world nodes).
fn fs_staged_trilinear(low: vec3<i32>, q: vec3<f32>) -> f32 {
    let fl = floor(q);
    let t = q - fl;
    let b = vec3<i32>(fl);
    let c000 = fs_staged(low, b);
    let c100 = fs_staged(low, b + vec3<i32>(1, 0, 0));
    let c010 = fs_staged(low, b + vec3<i32>(0, 1, 0));
    let c110 = fs_staged(low, b + vec3<i32>(1, 1, 0));
    let c001 = fs_staged(low, b + vec3<i32>(0, 0, 1));
    let c101 = fs_staged(low, b + vec3<i32>(1, 0, 1));
    let c011 = fs_staged(low, b + vec3<i32>(0, 1, 1));
    let c111 = fs_staged(low, b + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, t.x);
    let x10 = mix(c010, c110, t.x);
    let x01 = mix(c001, c101, t.x);
    let x11 = mix(c011, c111, t.x);
    return mix(mix(x00, x10, t.y), mix(x01, x11, t.y), t.z);
}

// The direction the field bends most at node: the Hessian's leading
// eigenvector (second differences a simulation cell apart, by power
// iteration). In the middle of a sheet or film the gradient is near nothing
// and points along the sheet; the bend still points across it.
fn fs_bend_axis(low: vec3<i32>, node: vec3<i32>, st: i32, c0: f32) -> vec3<f32> {
    let ex = vec3<i32>(st, 0, 0);
    let ey = vec3<i32>(0, st, 0);
    let ez = vec3<i32>(0, 0, st);
    let xx = fs_staged(low, node + ex) + fs_staged(low, node - ex) - 2.0 * c0;
    let yy = fs_staged(low, node + ey) + fs_staged(low, node - ey) - 2.0 * c0;
    let zz = fs_staged(low, node + ez) + fs_staged(low, node - ez) - 2.0 * c0;
    let xy = 0.25 * (fs_staged(low, node + ex + ey) - fs_staged(low, node + ex - ey)
        - fs_staged(low, node - ex + ey) + fs_staged(low, node - ex - ey));
    let xz = 0.25 * (fs_staged(low, node + ex + ez) - fs_staged(low, node + ex - ez)
        - fs_staged(low, node - ex + ez) + fs_staged(low, node - ex - ez));
    let yz = 0.25 * (fs_staged(low, node + ey + ez) - fs_staged(low, node + ey - ez)
        - fs_staged(low, node - ey + ez) + fs_staged(low, node - ey - ez));
    let hx = vec3<f32>(xx, xy, xz);
    let hy = vec3<f32>(xy, yy, yz);
    let hz = vec3<f32>(xz, yz, zz);
    // Start from the Hessian's longest column.
    var v = hx;
    if dot(hy, hy) > dot(v, v) { v = hy; }
    if dot(hz, hz) > dot(v, v) { v = hz; }
    for (var i = 0u; i < 4u; i = i + 1u) {
        let l = length(v);
        if l < 1.0e-12 { break; }
        v = v / l;
        v = v.x * hx + v.y * hy + v.z * hz;
    }
    return v;
}

@compute @workgroup_size(256)
fn cs_main(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
) {
    let slot = group.x;
    let h = params.cell_size;
    let far = FS_FAR_CELLS * h;
    if lane == 0u {
        atomicStore(&fs_band_any, 0u);
        fs_held = u32(slot < params.pool_tiles && ts_slot_entry(slot) != WT_NONE);
        if slot == params.pool_tiles {
            field[ts_canon()] = far;
        }
    }
    if workgroupUniformLoad(&fs_held) == 0u {
        if slot < params.pool_tiles {
            field[slot * 512u + lane] = far;
            field[slot * 512u + lane + FS_THREADS] = far;
        }
        return;
    }
    let band = params.band_cells * params.sim_cell_size;
    let first = slot * 512u + lane;
    if abs(reference[first]) < band || abs(reference[first + FS_THREADS]) < band {
        atomicOr(&fs_band_any, 1u);
    }
    workgroupBarrier();
    if lane == 0u {
        fs_in_band = atomicLoad(&fs_band_any);
    }
    if workgroupUniformLoad(&fs_in_band) == 0u {
        field[first] = prior[first];
        field[first + FS_THREADS] = prior[first + FS_THREADS];
        return;
    }
    if lane < WT_NEIGHBOURS {
        fs_near[lane] = ts_neighbour_slot(slot, lane);
    }
    workgroupBarrier();
    let s = params.stride;
    let halo = fs_halo();
    let side = fs_side();
    let low = ts_slot_tile(slot) * 8 - vec3<i32>(i32(halo));
    let canon = prior[ts_canon()];
    for (var i = lane; i < side * side * side; i = i + FS_THREADS) {
        let r = vec3<u32>(i % side, (i / side) % side, i / (side * side));
        let n = fs_slot(r);
        var v = canon;
        if n != WT_NONE {
            v = prior[n * 512u + ts_local(low + vec3<i32>(r))];
        }
        fs_a[i] = v;
    }
    workgroupBarrier();
    let home = ts_slot_tile(slot);
    let d = fs_ring_radius();
    let leash = params.smoothness * FS_LEASH_CELLS * params.sim_cell_size;
    for (var k = 0u; k < 2u; k = k + 1u) {
        let local = lane + k * FS_THREADS;
        let e = slot * 512u + local;
        let before = prior[e];
        let anchor = reference[e];
        var out = before;
        if abs(anchor) < band {
            let node = home * 8 + vec3<i32>(i32(local & 7u), i32((local >> 3u) & 7u), i32(local >> 6u));
            // The normal from central differences a simulation cell apart.
            let st = i32(s);
            let grad = vec3<f32>(
                fs_staged(low, node + vec3<i32>(st, 0, 0)) - fs_staged(low, node - vec3<i32>(st, 0, 0)),
                fs_staged(low, node + vec3<i32>(0, st, 0)) - fs_staged(low, node - vec3<i32>(0, st, 0)),
                fs_staged(low, node + vec3<i32>(0, 0, st)) - fs_staged(low, node - vec3<i32>(0, 0, st)),
            );
            // A distance field's gradient is about 1 at a surface and near
            // nothing in a sheet's or a gap's middle, where it points along
            // the sheet; under half, the bend gives the normal instead.
            var normal = grad;
            if params.keep_sheets != 0u && length(grad) < FS_WEAK_GRADIENT * 2.0 * f32(s) * h {
                normal = fs_bend_axis(low, node, st, before);
            }
            let gl = length(normal);
            if gl > 1.0e-9 {
                let nrm = normal / gl;
                let q = vec3<f32>(node);
                // Two directions square to the normal, from the axis it
                // leans on least; the ring's mean is the same whichever pair.
                let a = abs(nrm);
                var axis = vec3<f32>(0.0, 0.0, 1.0);
                if a.x <= a.y && a.x <= a.z {
                    axis = vec3<f32>(1.0, 0.0, 0.0);
                } else if a.y <= a.z {
                    axis = vec3<f32>(0.0, 1.0, 0.0);
                }
                let t1 = normalize(cross(nrm, axis));
                let t2 = cross(nrm, t1);
                var ring = 0.0;
                for (var i = 0u; i < 6u; i = i + 1u) {
                    let dir = FS_RING[i];
                    ring = ring + fs_staged_trilinear(low, q + d * (dir.x * t1 + dir.y * t2));
                }
                let along = 0.25 * before + 0.125 * ring;
                let w = weights[e];
                var delta = mix(0.5, 1.0, w) * (along - before);
                if params.keep_sheets != 0u && before < 0.0 {
                    let half = 0.5 * f32(s);
                    let middle = fs_staged_trilinear(low, q + nrm * half) > before
                        && fs_staged_trilinear(low, q - nrm * half) > before;
                    if middle {
                        delta = min(delta, -0.5 * before);
                    }
                }
                let reach = mix(1.0, 2.0, w) * leash;
                out = clamp(before + delta, anchor - reach, anchor + reach);
            }
        }
        field[e] = out;
    }
}
