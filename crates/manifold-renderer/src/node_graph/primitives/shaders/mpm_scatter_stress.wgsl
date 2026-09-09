// node.mpm_scatter_stress — HAND-AUTHORED standalone kernel.
//
// Density/stress (design step 4): reads the COMPLETED grid mass (dispatch
// ordering, not workgroup barriers, guarantees both scatter stages are
// sequential), reconstructs rho_p = sum(w*m_i)/h^3, writes it to a separate
// candidate particle record (never an in-place cross-thread mutation), and
// adds the stress momentum -4*dt*V_p/h^2 * w * sigma*d to the momentum cells
// with V_p = m_p/rho_p and sigma = -p*I + mu*(C + C^T). Mass cells are never
// modified by this stage. Momentum accumulation uses the same checked
// compare/exchange fixed-point path as the mass/momentum scatter.
//
// Codegen gap (reported, S4): mixed outputs from one invocation — an atomic
// grid output (out) plus a coincident particle output (particles_out) —
// exceeds what the generated buffer wrapper expresses (multi-output atoms
// are supported, but an atomic port among them is rejected outright). This
// kernel is the documented escape for atomic/global-dependency stages;
// `water_common.wgsl` is concatenated ahead so the math stays single-sourced.

struct WaterParticle {
    position_mass: vec4<f32>,
    velocity_density: vec4<f32>,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    previous_position: vec4<f32>,
}

struct Params {
    step_dt: f32,
    active_count: u32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_particles: array<WaterParticle>;
@group(0) @binding(2) var<storage, read_write> buf_accumulator: array<atomic<i32>>;
@group(0) @binding(3) var<storage, read_write> buf_status_out: array<atomic<u32>>;
@group(0) @binding(4) var<storage, read_write> buf_particles_out: array<WaterParticle>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.active_count) {
        return;
    }
    let p = buf_particles[idx];
    let m = p.position_mass.w;
    var out_rec = p;
    if (m == 0.0) {
        buf_particles_out[idx] = out_rec;
        return;
    }
    let pos = p.position_mass.xyz;
    if (!water_finite3(pos) || !water_finite1(m)) {
        atomicOr(&buf_status_out[0], WATER_FAULT_NONFINITE);
        buf_particles_out[idx] = out_rec;
        return;
    }
    let q = (pos - WATER_ORIGIN) * WATER_INV_H;
    if (!water_q_plausible(q)) {
        atomicOr(&buf_status_out[0], WATER_FAULT_OUTSIDE_DOMAIN);
        buf_particles_out[idx] = out_rec;
        return;
    }
    let base = water_stencil_base(q);
    if (!water_stencil_contained(base)) {
        atomicOr(&buf_status_out[0], WATER_FAULT_OUTSIDE_DOMAIN);
        buf_particles_out[idx] = out_rec;
        return;
    }
    let f = q - vec3<f32>(base);
    let wx = water_weights(f.x);
    let wy = water_weights(f.y);
    let wz = water_weights(f.z);

    // Reconstructed density from the completed mass accumulation. Mass is
    // read-only here — plain atomic loads, no adds.
    var rho = 0.0;
    for (var k = 0u; k < 3u; k = k + 1u) {
        for (var j = 0u; j < 3u; j = j + 1u) {
            for (var i = 0u; i < 3u; i = i + 1u) {
                let cell = water_grid_index(u32(base.x + i32(i)), u32(base.y + i32(j)), u32(base.z + i32(k)));
                let w = wx[i] * wy[j] * wz[k];
                rho = rho + w * f32(atomicLoad(&buf_accumulator[cell * 4u + 3u])) / WATER_FIXED_SCALE;
            }
        }
    }
    rho = rho / (WATER_H * WATER_H * WATER_H);
    out_rec.velocity_density = vec4<f32>(p.velocity_density.xyz, rho);
    if (!water_finite1(rho) || rho <= 0.0) {
        // rho <= 0 means the mass grid disagrees with the live particle —
        // report and skip the stress transfer (the record still carries the
        // bad density so validate can reject the candidate).
        atomicOr(&buf_status_out[0], WATER_FAULT_INVALID_DENSITY);
        buf_particles_out[idx] = out_rec;
        return;
    }

    let vel = p.velocity_density.xyz;
    let cx = p.affine_x.xyz;
    let cy = p.affine_y.xyz;
    let cz = p.affine_z.xyz;

    let vol = m / rho;
    let press = water_eos_pressure(rho);
    let factor = -4.0 * params.step_dt * vol / (WATER_H * WATER_H);

    for (var k = 0u; k < 3u; k = k + 1u) {
        for (var j = 0u; j < 3u; j = j + 1u) {
            for (var i = 0u; i < 3u; i = i + 1u) {
                let cell = water_grid_index(u32(base.x + i32(i)), u32(base.y + i32(j)), u32(base.z + i32(k)));
                let w = wx[i] * wy[j] * wz[k];
                let node = vec3<f32>(base) + vec3<f32>(f32(i), f32(j), f32(k));
                let d = node * WATER_H + WATER_ORIGIN - pos;
                let cd = vec3<f32>(dot(cx, d), dot(cy, d), dot(cz, d));
                // C^T * d: column i of C dotted with d. C rows are the
                // affine_x/y/z records.
                let ctd = vec3<f32>(
                    cx.x * d.x + cy.x * d.y + cz.x * d.z,
                    cx.y * d.x + cy.y * d.y + cz.y * d.z,
                    cx.z * d.x + cy.z * d.y + cz.z * d.z,
                );
                // sigma * d = -p*d + mu*(C*d + C^T*d)
                let sd = -press * d + WATER_MU * (cd + ctd);
                let stress = factor * w * sd;

                // Checked fixed-point accumulation — same CAS contract as the
                // mass/momentum scatter (see that kernel; inlined here because
                // WGSL user-function pointer parameters must be function
                // address space).
                var ok = true;
                var qmom = water_quantise(stress.x, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmom != 0) {
                    var cell_old = atomicLoad(&buf_accumulator[cell * 4u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmom)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_accumulator[cell * 4u], cell_old, cell_old + qmom);
                        if (r.exchanged) {
                            settled = true;
                            break;
                        }
                        cell_old = r.old_value;
                    }
                    if (!settled) {
                        atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    }
                }
                qmom = water_quantise(stress.y, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmom != 0) {
                    var cell_old = atomicLoad(&buf_accumulator[cell * 4u + 1u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmom)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_accumulator[cell * 4u + 1u], cell_old, cell_old + qmom);
                        if (r.exchanged) {
                            settled = true;
                            break;
                        }
                        cell_old = r.old_value;
                    }
                    if (!settled) {
                        atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    }
                }
                qmom = water_quantise(stress.z, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmom != 0) {
                    var cell_old = atomicLoad(&buf_accumulator[cell * 4u + 2u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmom)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_accumulator[cell * 4u + 2u], cell_old, cell_old + qmom);
                        if (r.exchanged) {
                            settled = true;
                            break;
                        }
                        cell_old = r.old_value;
                    }
                    if (!settled) {
                        atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    }
                }
            }
        }
    }
    buf_particles_out[idx] = out_rec;
}
