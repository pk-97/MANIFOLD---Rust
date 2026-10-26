// The sheeter's signed distance (gpu_flip_sheet_distance.rs): seeded from the
// liquid level set's zero crossings along grid edges, then swept with the
// Godunov update of |grad u| = 1 in Jacobi passes (A -> B, B -> A), clamped to
// `depth` metres. Cell centred, x fastest.
struct Params { nx: u32, ny: u32, nz: u32, count: u32, h: f32, depth: f32, pad0: u32, pad1: u32 };
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

fn clock_active() -> bool {
    return clock_plan[0].live_mode == 0u || clock_plan[0].step_dt > 0.0;
}

fn ts_cells() -> vec3<i32> { return vec3<i32>(i32(u.nx), i32(u.ny), i32(u.nz)); }
fn sign_of(v: f32) -> f32 { return select(-1.0, 1.0, v >= 0.0); }
// The face neighbour of `c` along `axis` by `d` (-1 or 1) as a flat index, or
// -1 off the grid.
fn neighbour(c: vec3<i32>, axis: u32, d: i32) -> i32 {
    var q = c;
    q[axis] = q[axis] + d;
    if any(q < vec3<i32>(0)) || any(q >= ts_cells()) { return -1; }
    return i32(ts_cell_at(q));
}
// Distance from the cell centre to the nearest zero crossing between it and a
// face neighbour of the other sign, or -1 when there is none.
fn crossing(c: vec3<i32>, p: f32) -> f32 {
    let s = sign_of(p);
    var best = -1.0;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        for (var k = 0; k < 2; k = k + 1) {
            let j = neighbour(c, axis, 2 * k - 1);
            if j < 0 { continue; }
            let pj = source[u32(j)];
            if sign_of(pj) == s { continue; }
            let t = u.h * abs(p) / (abs(p) + abs(pj));
            if best < 0.0 || t < best { best = t; }
        }
    }
    return best;
}

@compute @workgroup_size(256)
fn seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i = gid.x;
    if i >= u.count { return; }
    let p = source[i];
    let t = crossing(ts_cell_coord(i), p);
    field_out[i] = sign_of(p) * select(u.depth, t, t >= 0.0);
}

@compute @workgroup_size(256)
fn sweep(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i = gid.x;
    if i >= u.count { return; }
    let c = ts_cell_coord(i);
    let p = source[i];
    if crossing(c, p) >= 0.0 {
        field_out[i] = field_in[i];
        return;
    }
    let s = sign_of(p);
    var a = vec3<f32>(u.depth);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        for (var k = 0; k < 2; k = k + 1) {
            let j = neighbour(c, axis, 2 * k - 1);
            if j >= 0 && sign_of(source[u32(j)]) == s {
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
