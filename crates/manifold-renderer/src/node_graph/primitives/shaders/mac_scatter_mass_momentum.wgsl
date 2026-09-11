// node.mac_scatter_mass_momentum — hand-authored APIC MAC scatter.
//
// Equations 12–13 from Jiang et al. 2015.  The accumulator uses a common
// padded 65³ index, with six i32 Q20 slots per entry:
// [mass_x, momentum_x, mass_y, momentum_y, mass_z, momentum_z].

struct WaterParticle {
    position_mass: vec4<f32>,
    velocity_density: vec4<f32>,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    previous_position: vec4<f32>,
}

struct Params {
    active_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_particles: array<WaterParticle>;
@group(0) @binding(2) var<storage, read_write> buf_out: array<atomic<i32>>;
@group(0) @binding(3) var<storage, read_write> buf_status_out: array<atomic<u32>>;

const MAC_GRID_EDGE: u32 = 65u;
const MAC_FIXED_SCALE: f32 = 1048576.0;
const MAC_CAS_RETRIES: u32 = 2048u;

fn mac_face_offset(axis: u32) -> vec3<f32> {
    if (axis == 0u) {
        return vec3<f32>(0.0, 0.5, 0.5);
    }
    if (axis == 1u) {
        return vec3<f32>(0.5, 0.0, 0.5);
    }
    return vec3<f32>(0.5, 0.5, 0.0);
}

fn mac_face_dims(axis: u32) -> vec3<i32> {
    if (axis == 0u) {
        return vec3<i32>(65, 64, 64);
    }
    if (axis == 1u) {
        return vec3<i32>(64, 65, 64);
    }
    return vec3<i32>(64, 64, 65);
}

fn mac_grid_index(c: vec3<i32>) -> u32 {
    return u32(c.x) + MAC_GRID_EDGE * (u32(c.y) + MAC_GRID_EDGE * u32(c.z));
}

// A complete trilinear face stencil is required before any writes for this
// particle.  The conversion to i32 is safe after water_q_plausible bounds q.
fn mac_stencil_base(pos: vec3<f32>, axis: u32, base: ptr<function, vec3<i32>>) -> bool {
    let q = (pos - WATER_ORIGIN) * WATER_INV_H - mac_face_offset(axis);
    if (!water_finite3(q) || !water_q_plausible(q)) {
        return false;
    }
    let b = vec3<i32>(floor(q));
    let dims = mac_face_dims(axis);
    if (any(b < vec3<i32>(0)) || any(b + vec3<i32>(1) >= dims)) {
        return false;
    }
    *base = b;
    return true;
}

fn mac_checked_add(index: u32, contribution: i32) {
    if (contribution == 0) {
        return;
    }
    var old = atomicLoad(&buf_out[index]);
    for (var attempt = 0u; attempt < MAC_CAS_RETRIES; attempt = attempt + 1u) {
        if (water_add_overflows(old, contribution)) {
            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
            return;
        }
        let next = old + contribution;
        let result = atomicCompareExchangeWeak(&buf_out[index], old, next);
        if (result.exchanged) {
            return;
        }
        old = result.old_value;
    }
    // A bounded retry is a data fault: dropping the contribution retains the
    // last representable value and prevents an unbounded live-show spin.
    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
}

fn mac_scatter_component(
    p: WaterParticle,
    axis: u32,
    base: vec3<i32>,
    frac: vec3<f32>,
) {
    let wx = vec2<f32>(1.0 - frac.x, frac.x);
    let wy = vec2<f32>(1.0 - frac.y, frac.y);
    let wz = vec2<f32>(1.0 - frac.z, frac.z);
    let c = array<vec4<f32>, 3>(p.affine_x, p.affine_y, p.affine_z);
    let pos = p.position_mass.xyz;
    let mass = p.position_mass.w;
    let face_offset = mac_face_offset(axis);

    for (var z = 0u; z < 2u; z = z + 1u) {
        for (var y = 0u; y < 2u; y = y + 1u) {
            for (var x = 0u; x < 2u; x = x + 1u) {
                let cell = base + vec3<i32>(i32(x), i32(y), i32(z));
                let weight = wx[x] * wy[y] * wz[z];
                let face_pos = WATER_ORIGIN + WATER_H * (vec3<f32>(cell) + face_offset);
                let affine_velocity = dot(c[axis].xyz, face_pos - pos);
                let mass_contribution = mass * weight;
                let momentum_contribution = mass_contribution * (p.velocity_density[axis] + affine_velocity);
                var ok = true;
                let qmass = water_quantise(mass_contribution, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                } else {
                    let slot = mac_grid_index(cell) * 6u + axis * 2u;
                    mac_checked_add(slot, qmass);
                }
                ok = true;
                let qmomentum = water_quantise(momentum_contribution, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                } else {
                    let slot = mac_grid_index(cell) * 6u + axis * 2u + 1u;
                    mac_checked_add(slot, qmomentum);
                }
            }
        }
    }
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.active_count) {
        return;
    }
    let p = buf_particles[idx];
    let mass = p.position_mass.w;
    if (mass == 0.0) {
        return;
    }
    let pos = p.position_mass.xyz;
    let vel = p.velocity_density.xyz;
    let affine = array<vec4<f32>, 3>(p.affine_x, p.affine_y, p.affine_z);
    if (!water_finite3(pos) || !water_finite1(mass) || !water_finite3(vel)
        || !water_finite3(affine[0].xyz) || !water_finite3(affine[1].xyz)
        || !water_finite3(affine[2].xyz)) {
        atomicOr(&buf_status_out[0], WATER_FAULT_NONFINITE);
        return;
    }
    if (mass < 0.0) {
        atomicOr(&buf_status_out[0], WATER_FAULT_INVALID_DENSITY);
        return;
    }

    // Validate all three 8-face stencils first, so a bad particle never
    // partially replaces the accumulator.
    var bases = array<vec3<i32>, 3>(vec3<i32>(0), vec3<i32>(0), vec3<i32>(0));
    var fractions = array<vec3<f32>, 3>(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0));
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        // Naga SPIR-V cannot cache a dynamically indexed pointer argument.
        // Pass a local pointer, then store the same validated base.
        var base = vec3<i32>(0);
        if (!mac_stencil_base(pos, axis, &base)) {
            atomicOr(&buf_status_out[0], WATER_FAULT_OUTSIDE_DOMAIN);
            return;
        }
        bases[axis] = base;
        fractions[axis] = (pos - WATER_ORIGIN) * WATER_INV_H - mac_face_offset(axis)
            - vec3<f32>(bases[axis]);
    }
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        mac_scatter_component(p, axis, bases[axis], fractions[axis]);
    }
}
