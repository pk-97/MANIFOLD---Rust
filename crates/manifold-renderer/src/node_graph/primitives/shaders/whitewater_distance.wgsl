// FLIP Fluids particlelevelset.cpp::calculateCurvatureGrid and
// levelsetsolver.cpp::reinitializeUpwind (MIT; see THIRD_PARTY_NOTICES.md).
// Stage-only valid-band construction and convergence reduction.
struct Params { nx: u32, ny: u32, nz: u32, count: u32, h: f32, iteration: u32, bx: u32, by: u32 };
@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> source: array<f32>;
@group(0) @binding(2) var<storage, read_write> current: array<f32>;
@group(0) @binding(3) var<storage, read> candidate: array<f32>;
@group(0) @binding(4) var<storage, read_write> valid: array<u32>;
@group(0) @binding(5) var<storage, read_write> blocks: array<u32>;
@group(0) @binding(6) var<storage, read_write> state: array<atomic<u32>>;
// The FLIP clock plan (gpu_flip_step.wgsl ClockPlan). An inactive substep slot
// (live clock, zero step) does no work; a zero plan is always active.
struct ClockPlan {
    step_dt: f32, elapsed: f32, remaining: f32, maximum_speed: f32,
    cap_hit: u32, nonfinite: u32, step_index: u32, event: u32,
    numerical_end: f32, marker_limit: f32, _pad0: u32, live_mode: u32,
};
@group(0) @binding(7) var<storage, read> clock_plan: array<ClockPlan>;
// The upwind sweep's indirect grid: zero groups in an inactive slot.
@group(0) @binding(8) var<storage, read_write> sweep_args: array<u32>;
fn clock_active() -> bool {
    return clock_plan[0].live_mode == 0u || clock_plan[0].step_dt > 0.0;
}
@compute @workgroup_size(1)
fn gate() {
    sweep_args[0] = select(0u, (u.count + 255u) / 256u, clock_active());
    sweep_args[1] = 1u;
    sweep_args[2] = 1u;
}
@compute @workgroup_size(256)
fn mark_blocks(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let bz = (u.nz+5u)/6u;
    if gid.x >= u.bx*u.by*bz { return; }
    let base = vec3<u32>(gid.x%u.bx, gid.x/u.bx%u.by, gid.x/(u.bx*u.by))*6u;
    var hit = 0u;
    for (var k=0u;k<6u;k=k+1u) { for (var j=0u;j<6u;j=j+1u) { for (var i=0u;i<6u;i=i+1u) {
        let c = base+vec3<u32>(i,j,k);
        if c.x<u.nx && c.y<u.ny && c.z<u.nz {
            hit = max(hit,u32(abs(source[c.x+u.nx*(c.y+u.ny*c.z)])<2.0*u.h));
        }
    } } }
    blocks[gid.x]=hit;
}
@compute @workgroup_size(256)
fn initialize(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let i=gid.x; if i>=u.count { return; }
    let c=vec3<i32>(vec3<u32>(i%u.nx,i/u.nx%u.ny,i/(u.nx*u.ny)))/6;
    let dims=vec3<i32>(i32(u.bx),i32(u.by),i32((u.nz+5u)/6u));
    var hit=blocks[u32(c.x+dims.x*(c.y+dims.y*c.z))];
    for(var a=0u;a<3u;a=a+1u) { for(var d=-1;d<=1;d=d+2) {
        var q=c; q[a]=q[a]+d;
        if all(q>=vec3<i32>(0)) && all(q<dims) { hit=max(hit,blocks[u32(q.x+dims.x*(q.y+dims.y*q.z))]); }
    } }
    valid[i]=hit; current[i]=source[i];
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
