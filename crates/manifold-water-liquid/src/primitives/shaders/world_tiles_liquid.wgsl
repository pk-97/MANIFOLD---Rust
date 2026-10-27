// The points of a world tiles build from liquid particle records
// (`FluidParticle`, 32 bytes: position_radius, then velocity and id), bound
// at 13: a record among the slots in use is live when its radius is
// positive, binned by `ts_world_cell` like every other particle pass.

@group(0) @binding(13) var<storage, read> wt_particles: array<vec4<f32>>;
// The slots in use, [groups, 1, 1, n]: the point passes dispatch from the
// triple, and records from n on may hold anything.
@group(0) @binding(15) var<storage, read> wt_record: array<u32>;

fn wt_point_live(i: u32) -> bool {
    return i < wt_record[3] && wt_particles[2u * i].w > 0.0;
}

fn wt_point_cell(i: u32) -> vec3<i32> {
    return ts_world_cell(wt_particles[2u * i].xyz, wt.origin, wt.inv_cell);
}
