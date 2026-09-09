// node.seed_water — fusable BUFFER body, SOURCE. Seeds the static pool
// lattice: cell-centred h/2-spacing grid inside the configured pool box,
// velocity and affine state zero, density at rest, mass rho0*(h/2)^3.
// Slots past the seed count are written as zero records (mass zero =
// inactive) so the whole capacity is initialised in one dispatch.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the WaterParticle element written to
// buf_out[idx]. The codegen synthesizes `struct Element` with the
// WATER_PARTICLE_SPECS field order (position_mass, velocity_density,
// affine_x, affine_y, affine_z, previous_position). `max_capacity` is the
// allocation-only convention param — accepted and ignored. `water_common.wgsl`
// is prepended via wgsl_includes.
fn body(
    idx: u32,
    count: u32,
    pool_min_x: f32,
    pool_min_y: f32,
    pool_min_z: f32,
    pool_max_x: f32,
    pool_max_y: f32,
    pool_max_z: f32,
    grid_spacing: f32,
    rest_density: f32,
    max_capacity: i32,
) -> Element {
    let spacing = grid_spacing * 0.5; // S1 SEED_LATTICE_DIVISOR = 2
    let nx = u32(floor((pool_max_x - pool_min_x) / spacing + 0.5));
    let ny = u32(floor((pool_max_y - pool_min_y) / spacing + 0.5));
    let nz = u32(floor((pool_max_z - pool_min_z) / spacing + 0.5));
    let seed_count = nx * ny * nz;
    if (idx >= seed_count) {
        return Element(
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
            vec4<f32>(0.0),
        );
    }
    // Row-major lattice: idx = ix + nx * (iy + ny * iz), cell-centred.
    let ix = idx % nx;
    let iy = (idx / nx) % ny;
    let iz = idx / (nx * ny);
    let pos = vec3<f32>(pool_min_x, pool_min_y, pool_min_z)
        + (vec3<f32>(f32(ix), f32(iy), f32(iz)) + vec3<f32>(0.5)) * spacing;
    let mass = rest_density * spacing * spacing * spacing; // S1 PARTICLE_MASS
    return Element(
        vec4<f32>(pos, mass),
        vec4<f32>(0.0, 0.0, 0.0, rest_density),
        vec4<f32>(0.0),
        vec4<f32>(0.0),
        vec4<f32>(0.0),
        vec4<f32>(pos, 0.0),
    );
}
