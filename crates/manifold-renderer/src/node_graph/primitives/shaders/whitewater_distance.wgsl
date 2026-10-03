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
@compute @workgroup_size(256)
fn mark_blocks(@builtin(global_invocation_id) gid: vec3<u32>) {
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
    if u.iteration==0u { atomicStore(&state[0],bitcast<u32>(-1.0)); atomicStore(&state[2],0u); }
    atomicStore(&state[1],0u);
}
@compute @workgroup_size(256)
fn reduce(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i=gid.x; if i>=u.count || valid[i]==0u { return; }
    atomicMax(&state[1],bitcast<u32>(abs(candidate[i]-current[i])));
}
@compute @workgroup_size(1)
fn decide() {
    let diff=bitcast<f32>(atomicLoad(&state[1]));
    let last=bitcast<f32>(atomicLoad(&state[0]));
    if abs(diff-last)<0.01*u.h || u.iteration==5u { atomicStore(&state[2],1u); }
    atomicStore(&state[0],bitcast<u32>(diff));
}
@compute @workgroup_size(256)
fn accept(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i=gid.x; if i>=u.count { return; }
    if atomicLoad(&state[2])==0u { current[i]=candidate[i]; }
}
@compute @workgroup_size(256)
fn finish(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i=gid.x; if i>=u.count { return; }
    if valid[i]==0u { current[i]=5.0*u.h; }
}
