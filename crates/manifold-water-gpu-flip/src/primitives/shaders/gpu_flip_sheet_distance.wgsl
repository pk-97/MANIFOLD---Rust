// The sheeter's signed distance (gpu_flip_sheet_distance.rs): seeded from the
// liquid level set's zero crossings along grid edges, then swept with the
// Godunov update of |grad u| = 1 in Jacobi passes (A -> B, B -> A), clamped to
// `depth` metres. Cell centred; `source`, `field_in` and `field_out` are
// pooled cell fields on world tiles (world_tiles_read.wgsl). The seed also
// keeps each cell's mask: whether it sits beside a crossing (its value is
// final) and which face neighbours are held and of its sign, the only ones a
// sweep reads, so no sweep looks for crossings or signs again. Every kernel runs
// over the cells of the held slots, 512 threads a slot, and sees a tile that
// holds no slot as no neighbour at all.
struct Params { h: f32, depth: f32, pool_tiles: u32, _pad_h: u32 };
// The FLIP clock plan (gpu_flip_step/common.wgsl ClockPlan). An inactive substep slot
// (live clock, zero step) does no work.
struct ClockPlan {
    step_dt: f32, elapsed: f32, remaining: f32, maximum_speed: f32,
    cap_hit: u32, nonfinite: u32, step_index: u32, event: u32,
    numerical_end: f32, marker_limit: f32, _pad0: u32, live_mode: u32,
};
@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> source: array<f32>;
@group(0) @binding(2) var<storage, read> field_in: array<f32>;
@group(0) @binding(3) var<storage, read_write> field_out: array<f32>;
@group(0) @binding(4) var<storage, read> clock_plan: array<ClockPlan>;
// The world tile table.
@group(0) @binding(5) var<storage, read> ts_table: array<u32>;
// The held slots the dispatch covers.
@group(0) @binding(6) var<storage, read> slots: array<u32>;
// The solid's distance at the cell corners (gpu_flip_step `solid`), read by
// `through_solids` only.
@group(0) @binding(7) var<storage, read> solid: array<f32>;
// Per cell, from `seed`: SEEDED when it lies beside a zero crossing, and bit
// 2 axis + k when its face neighbour along `axis` by 2k - 1 is held and of
// its sign.
@group(0) @binding(8) var<storage, read_write> masks: array<u32>;
const SEEDED: u32 = 64u;

fn clock_active() -> bool {
    return clock_plan[0].live_mode == 0u || clock_plan[0].step_dt > 0.0;
}

fn ts_pool_tiles() -> u32 { return u.pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }
fn sign_of(v: f32) -> f32 { return select(-1.0, 1.0, v >= 0.0); }
// The face neighbour of `c` along `axis` by `d` (-1 or 1) as a pool index, or
// -1 where its tile holds no slot.
fn neighbour(c: vec3<i32>, axis: u32, d: i32) -> i32 {
    var q = c;
    q[axis] = q[axis] + d;
    let at = ts_cell_at(q);
    if at == ts_canon() { return -1; }
    return i32(at);
}

// The sheeter's source: the level set with every cell whose centre is inside
// a solid at most -h/2, so a solid reads as water. Water lying on a floor or
// against a wall is then as deep as the solid behind it, and only water with
// air on both sides is thin; with the solid as air one cell in, every pool
// shallower than the depth walk read as one sheet and seeded everywhere.
@compute @workgroup_size(256)
fn through_solids(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i = ts_list_cell(slots[gid.x >> 9u], gid.x);
    ts_home(i);
    let c = ts_cell_coord(i);
    var sum = 0.0;
    for (var k = 0; k < 8; k = k + 1) {
        sum = sum + solid[ts_cell_at(c + vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1))];
    }
    var p = source[i];
    // gpu_flip_step/solids.wgsl centre_in_solid.
    if 0.125 * sum < 1e-3 * u.h {
        p = min(p, -0.5 * u.h);
    }
    field_out[i] = p;
}

@compute @workgroup_size(256)
fn seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i = ts_list_cell(slots[gid.x >> 9u], gid.x);
    ts_home(i);
    let c = ts_cell_coord(i);
    let p = source[i];
    let s = sign_of(p);
    // The distance from the cell centre to the nearest zero crossing between
    // it and a face neighbour of the other sign, -1 when there is none.
    var t = -1.0;
    var mask = 0u;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        for (var k = 0; k < 2; k = k + 1) {
            let j = neighbour(c, axis, 2 * k - 1);
            if j < 0 { continue; }
            let pj = source[u32(j)];
            if sign_of(pj) == s {
                mask = mask | (1u << (2u * axis + u32(k)));
                continue;
            }
            let at = u.h * abs(p) / (abs(p) + abs(pj));
            if t < 0.0 || at < t { t = at; }
        }
    }
    masks[i] = mask | select(0u, SEEDED, t >= 0.0);
    field_out[i] = s * select(u.depth, t, t >= 0.0);
}

@compute @workgroup_size(256)
fn sweep(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i = ts_list_cell(slots[gid.x >> 9u], gid.x);
    ts_home(i);
    let mask = masks[i];
    if (mask & SEEDED) != 0u {
        field_out[i] = field_in[i];
        return;
    }
    let c = ts_cell_coord(i);
    let s = sign_of(source[i]);
    var a = vec3<f32>(u.depth);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        for (var k = 0; k < 2; k = k + 1) {
            if (mask & (1u << (2u * axis + u32(k)))) != 0u {
                let j = neighbour(c, axis, 2 * k - 1);
                a[axis] = min(a[axis], abs(field_in[u32(j)]));
            }
        }
    }
    let lo = min(a.x, min(a.y, a.z));
    let hi = max(a.x, max(a.y, a.z));
    let a1 = lo;
    let a2 = max(min(a.x, a.y), min(max(a.x, a.y), a.z));
    let a3 = hi;
    let h = u.h;
    var v = a1 + h;
    if v > a2 {
        v = (a1 + a2 + sqrt(max(2.0 * h * h - (a1 - a2) * (a1 - a2), 0.0))) / 2.0;
        if v > a3 {
            let sum = a1 + a2 + a3;
            v = (sum + sqrt(max(sum * sum - 3.0 * (a1 * a1 + a2 * a2 + a3 * a3 - h * h), 0.0))) / 3.0;
        }
    }
    field_out[i] = s * min(abs(field_in[i]), min(v, u.depth));
}
