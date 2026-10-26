// The points of a world tiles build from liquid particle records
// (`FluidParticle`, 32 bytes: position_radius, then velocity and id), bound
// at 13: live when the radius is positive, binned by `ts_world_cell` like
// every other particle pass.

@group(0) @binding(13) var<storage, read> wt_particles: array<vec4<f32>>;

fn wt_point_live(i: u32) -> bool {
    return wt_particles[2u * i].w > 0.0;
}

fn wt_point_cell(i: u32) -> vec3<i32> {
    return ts_world_cell(wt_particles[2u * i].xyz, wt.origin, wt.inv_cell);
}
