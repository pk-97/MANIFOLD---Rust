// node.scatter_particles_3d — BUFFER body (freeze §12, buffer domain), ATOMIC
// 3D SCATTER. Each live particle atomic-adds `scaled_energy` to its nearest
// voxel in a vol_res × vol_res × vol_depth accumulator. Matches
// fluid_scatter_3d.wgsl `splat_3d`.
//
// ABI: `particles` (Particle) coincident → e_particles. The `accum` output is
// an ATOMIC u32 volume accumulator (`atomic_outputs: ["accum"]`) — the body
// `atomicAdd`s into the `buf_accum` global; the wrapper calls `body(...)` as a
// statement (no single-element return). `vol_res` (XY) / `vol_depth` (Z) /
// `scaled_energy` are i32 params; positions outside [0,1]³ are toroidally
// wrapped (% vr / % vd). `active_count` (i32) is unused here — the wrapper's
// dispatch_count guard bounds the splat. An early return on a dead particle
// reproduces the hand kernel's no-write. Element = the Particle struct.
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    active_count: i32,
    vol_res: i32,
    vol_depth: i32,
    scaled_energy: i32,
) {
    if e_particles.life <= 0.0 {
        return;
    }
    let p = e_particles.position;
    let vr = u32(vol_res);
    let vd = u32(vol_depth);
    let coord = vec3<u32>(
        u32(p.x * f32(vr)) % vr,
        u32(p.y * f32(vr)) % vr,
        u32(p.z * f32(vd)) % vd,
    );
    let cell = coord.z * vr * vr + coord.y * vr + coord.x;
    atomicAdd(&buf_accum[cell], u32(scaled_energy));
}
