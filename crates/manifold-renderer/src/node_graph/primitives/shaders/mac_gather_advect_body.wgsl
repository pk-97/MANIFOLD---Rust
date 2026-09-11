// The columns of affine hold the three component gradients (APIC rows).
struct MacGatherSample {
    velocity: vec3<f32>,
    affine: mat3x3<f32>,
    valid: bool,
}

fn mac_gather_sample(pos: vec3<f32>) -> MacGatherSample {
    var sample = MacGatherSample(vec3<f32>(0.0), mat3x3<f32>(
        vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0)), false);
    if (!water_finite3(pos)) { return sample; }
    let cell_q = (pos - WATER_ORIGIN) * WATER_INV_H;
    if (!water_q_plausible(cell_q)) { return sample; }
    for (var axis = 0u; axis < 3u; axis++) {
        var offset = vec3<f32>(0.5);
        offset[axis] = 0.0;
        let q = cell_q - offset;
        var dims = vec3<i32>(64);
        dims[axis] = 65;
        let base = vec3<i32>(floor(q));
        if (any(base < vec3<i32>(0)) || any(base + vec3<i32>(1) >= dims)) {
            return sample;
        }
        let fraction = q - vec3<f32>(base);
        var velocity = 0.0;
        var gradient = vec3<f32>(0.0);
        for (var corner = 0u; corner < 8u; corner++) {
            let bit = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
            let coordinate = base + vec3<i32>(bit);
            let cell = u32(coordinate.x) + 65u * (u32(coordinate.y) + 65u * u32(coordinate.z));
            let face = buf_grid[cell];
            let valid = face.mac_valid[axis];
            let u = face.mac_velocity[axis];
            // All eight faces are required, including zero-weight corners:
            // their gradients can still contribute to the affine transfer.
            if (!water_finite1(valid) || valid <= 0.0 || !water_finite1(u)) {
                return sample;
            }
            let weight = select(vec3<f32>(1.0) - fraction, fraction, bit != vec3<u32>(0u));
            let sign = select(vec3<f32>(-1.0), vec3<f32>(1.0), bit != vec3<u32>(0u));
            velocity += weight.x * weight.y * weight.z * u;
            gradient += WATER_INV_H * u * sign * vec3<f32>(
                weight.y * weight.z, weight.x * weight.z, weight.x * weight.y);
        }
        if (!water_finite1(velocity) || !water_finite3(gradient)) { return sample; }
        sample.velocity[axis] = velocity;
        sample.affine[axis] = gradient;
    }
    sample.valid = true;
    return sample;
}

fn mac_gather_reject(particle: Element) -> Element {
    var out = particle;
    out.position_mass = vec4<f32>(vec3<f32>(bitcast<f32>(0x7fc00000u)), particle.position_mass.w);
    return out;
}

fn body(idx: u32, count: u32, e_particles: Element, step_dt: f32) -> Element {
    let mass = e_particles.position_mass.w;
    if (mass == 0.0) { return e_particles; }
    if (!water_finite1(mass) || mass < 0.0 || !water_finite1(e_particles.velocity_density.w)
        || !water_finite1(step_dt) || step_dt < 0.0) {
        return mac_gather_reject(e_particles);
    }
    let pos = e_particles.position_mass.xyz;
    let first = mac_gather_sample(pos);
    if (!first.valid) { return mac_gather_reject(e_particles); }
    let second = mac_gather_sample(pos + 0.5 * step_dt * first.velocity);
    if (!second.valid) { return mac_gather_reject(e_particles); }
    let third = mac_gather_sample(pos + 0.75 * step_dt * second.velocity);
    if (!third.valid) { return mac_gather_reject(e_particles); }
    let next = pos + step_dt * (2.0 * first.velocity + 3.0 * second.velocity + 4.0 * third.velocity) / 9.0;
    if (!water_finite3(next)) { return mac_gather_reject(e_particles); }

    var out = e_particles;
    out.velocity_density = vec4<f32>(first.velocity, e_particles.velocity_density.w);
    out.affine_x = vec4<f32>(first.affine[0], 0.0);
    out.affine_y = vec4<f32>(first.affine[1], 0.0);
    out.affine_z = vec4<f32>(first.affine[2], 0.0);
    out.previous_position = vec4<f32>(pos, 0.0);
    out.position_mass = vec4<f32>(next, mass);
    return out;
}
