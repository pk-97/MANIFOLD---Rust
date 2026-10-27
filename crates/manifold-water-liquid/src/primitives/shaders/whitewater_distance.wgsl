// FLIP Fluids particlelevelset.cpp::calculateCurvatureGrid and
// levelsetsolver.cpp::reinitializeUpwind (MIT; see THIRD_PARTY_NOTICES.md).
// Stage-only valid-band construction and convergence reduction over pooled
// cell arrays on the step's world tiles (world_tiles_read.wgsl composes
// before this file): `source`, `current`, `candidate` and `valid` hold one
// element per pool index, the canonical element last.
struct Params { count: u32, pool_tiles: u32, entries: u32, iteration: u32, h: f32, _pad0: u32, _pad1: u32, _pad2: u32 };
@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> source: array<f32>;
@group(0) @binding(2) var<storage, read_write> current: array<f32>;
@group(0) @binding(3) var<storage, read> candidate: array<f32>;
@group(0) @binding(4) var<storage, read_write> valid: array<u32>;
// One flag per pool slot: 1 when a cell of the slot's tile is within 2h of
// the surface.
@group(0) @binding(5) var<storage, read_write> slot_flags: array<u32>;
@group(0) @binding(6) var<storage, read_write> state: array<atomic<u32>>;
// The FLIP clock plan (gpu_flip_step/common.wgsl ClockPlan). An inactive substep slot
// (live clock, zero step) does no work; a zero plan is always active.
struct ClockPlan {
    step_dt: f32, elapsed: f32, remaining: f32, maximum_speed: f32,
    cap_hit: u32, nonfinite: u32, step_index: u32, event: u32,
    numerical_end: f32, marker_limit: f32, _pad0: u32, live_mode: u32,
};
@group(0) @binding(7) var<storage, read> clock_plan: array<ClockPlan>;
// The upwind sweep's indirect grid: zero groups in an inactive slot.
@group(0) @binding(8) var<storage, read_write> sweep_args: array<u32>;
// The world tile table.
@group(0) @binding(9) var<storage, read> ts_table: array<u32>;

fn ts_pool_tiles() -> u32 { return u.pool_tiles; }
fn ts_table_entries() -> u32 { return u.entries; }
fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }

fn clock_active() -> bool {
    return clock_plan[0].live_mode == 0u || clock_plan[0].step_dt > 0.0;
}
@compute @workgroup_size(1)
fn gate() {
    sweep_args[0] = select(0u, (u.count + 255u) / 256u, clock_active());
    sweep_args[1] = 1u;
    sweep_args[2] = 1u;
}
// One thread per pool slot: its flag, 0 for a free slot.
@compute @workgroup_size(256)
fn mark_slots(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let slot = gid.x;
    if slot >= u.pool_tiles { return; }
    var hit = 0u;
    if ts_slot_entry(slot) != WT_NONE {
        for (var l = 0u; l < 512u; l = l + 1u) {
            hit = max(hit, u32(abs(source[slot * 512u + l]) < 2.0 * u.h));
        }
    }
    slot_flags[slot] = hit;
}
// A cell is valid in a flagged slot and in a slot sharing a face with one.
// The canonical element and a free slot are not valid.
@compute @workgroup_size(256)
fn initialize(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i = gid.x; if i >= u.count { return; }
    current[i] = source[i];
    let slot = i >> 9u;
    if slot >= u.pool_tiles || ts_slot_entry(slot) == WT_NONE {
        valid[i] = 0u;
        return;
    }
    var hit = slot_flags[slot];
    for (var a = 0u; a < 3u; a = a + 1u) { for (var d = -1; d <= 1; d = d + 2) {
        var step = vec3<i32>(0);
        step[a] = d;
        let neighbour = ts_neighbour_slot(slot, ts_neighbour_index(step));
        if neighbour != WT_NONE { hit = max(hit, slot_flags[neighbour]); }
    } }
    valid[i]=hit;
}
@compute @workgroup_size(1)
fn reset() {
    if !clock_active() { return; }
    if u.iteration==0u { atomicStore(&state[0],bitcast<u32>(-1.0)); atomicStore(&state[2],0u); }
    atomicStore(&state[1],0u);
}
@compute @workgroup_size(256)
fn reduce(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i=gid.x; if i>=u.count || valid[i]==0u { return; }
    atomicMax(&state[1],bitcast<u32>(abs(candidate[i]-current[i])));
}
@compute @workgroup_size(1)
fn decide() {
    if !clock_active() { return; }
    let diff=bitcast<f32>(atomicLoad(&state[1]));
    let last=bitcast<f32>(atomicLoad(&state[0]));
    if abs(diff-last)<0.01*u.h || u.iteration==5u { atomicStore(&state[2],1u); }
    atomicStore(&state[0],bitcast<u32>(diff));
}
@compute @workgroup_size(256)
fn accept(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i=gid.x; if i>=u.count { return; }
    if atomicLoad(&state[2])==0u { current[i]=candidate[i]; }
}
@compute @workgroup_size(256)
fn finish(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i=gid.x; if i>=u.count { return; }
    if valid[i]==0u { current[i]=5.0*u.h; }
}
