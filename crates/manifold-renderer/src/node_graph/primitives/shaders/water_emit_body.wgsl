// node.water_emit — fusable BUFFER body, coincident. Deterministic lattice
// birth into the unused tail of the particle wire (design step 1): slots in
// the absolute range [birth_lo, birth_hi) that are still inactive (mass zero)
// become cell-centred h/2-spacing lattice records inside the emission box —
// zero velocity, rest density, particle mass rho0*(h/2)^3. Ordinal births,
// no GPU append/readback: the per-owner cursor lives CPU-side in node.water_emit
// and the birth window arrives as derived uniforms. Existing water — seeded
// prefix or already-born tail — passes through untouched.
//
// ABI (buffer standalone codegen): `in` is coincident (pre-read into
// e_particles). `rate` is the port-shadowed emission rate (the cursor consumes
// it CPU-side; the body only needs the resolved birth window). `first_free` is
// the first unused slot (where the seed lattice ends). `birth_lo`/`birth_hi`
// are the derived u32 window packed per dispatch by run().
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    emit_min_x: f32,
    emit_min_y: f32,
    emit_min_z: f32,
    emit_max_x: f32,
    emit_max_y: f32,
    emit_max_z: f32,
    grid_spacing: f32,
    rest_density: f32,
    rate: f32,
    first_free: i32,
    birth_lo: u32,
    birth_hi: u32,
) -> Element {
    var out = e_particles;
    if (idx < birth_lo || idx >= birth_hi) {
        return out;
    }
    // Safety belt: the cursor hands out each ordinal once, so a live slot in
    // the window means the pool was re-seeded out from under it — never
    // overwrite existing water.
    if (out.position_mass.w != 0.0) {
        return out;
    }
    let ordinal = idx - u32(first_free);
    let spacing = grid_spacing * 0.5; // S1 SEED_LATTICE_DIVISOR = 2
    let nx = u32(floor((emit_max_x - emit_min_x) / spacing + 0.5));
    let ny = u32(floor((emit_max_y - emit_min_y) / spacing + 0.5));
    let nz = u32(floor((emit_max_z - emit_min_z) / spacing + 0.5));
    // Row-major lattice, cell-centred — the same layout node.seed_water uses,
    // so emitted water continues the lattice seamlessly.
    let ix = ordinal % nx;
    let iy = (ordinal / nx) % ny;
    let iz = ordinal / (nx * ny);
    let pos = vec3<f32>(emit_min_x, emit_min_y, emit_min_z)
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
