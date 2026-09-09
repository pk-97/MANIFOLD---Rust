// node.mpm_scatter_mass_momentum — HAND-AUTHORED standalone kernel.
//
// P2G mass/momentum (design step 3): each live particle adds w*m to the mass
// cell and w*m*(v + C*d) to the momentum cells of its 27 stencil nodes, as
// signed fixed-point Q = 2^20 via checked compare/exchange accumulation.
// Overflow (or bounded-retry exhaustion) sticks FAULT_INTEGER_OVERFLOW and
// retains the last representable cell value — a wrapped atomicAdd is never
// accepted as data (design D6).
//
// Codegen gap (reported, S4): the generated buffer wrapper cannot express
// this stage — it has TWO atomic outputs (the i32 accumulator AND the u32
// status word); generate_standalone_buffer rejects any multi-output atom
// carrying an atomic port. This kernel is the documented escape for
// atomic/global-dependency stages; `water_common.wgsl` is concatenated ahead
// so the math stays single-sourced with the fusable stages.

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

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.active_count) {
        return;
    }
    let p = buf_particles[idx];
    let m = p.position_mass.w;
    if (m == 0.0) {
        return; // inactive slot
    }
    let pos = p.position_mass.xyz;
    if (!water_finite3(pos) || !water_finite1(m)) {
        atomicOr(&buf_status_out[0], WATER_FAULT_NONFINITE);
        return;
    }
    let q = (pos - WATER_ORIGIN) * WATER_INV_H;
    if (!water_q_plausible(q)) {
        atomicOr(&buf_status_out[0], WATER_FAULT_OUTSIDE_DOMAIN);
        return;
    }
    let base = water_stencil_base(q);
    if (!water_stencil_contained(base)) {
        // Never clip stencil mass at a grid edge — fault and skip.
        atomicOr(&buf_status_out[0], WATER_FAULT_OUTSIDE_DOMAIN);
        return;
    }
    let f = q - vec3<f32>(base);
    let wx = water_weights(f.x);
    let wy = water_weights(f.y);
    let wz = water_weights(f.z);

    let vel = p.velocity_density.xyz;
    let cx = p.affine_x.xyz;
    let cy = p.affine_y.xyz;
    let cz = p.affine_z.xyz;

    for (var k = 0u; k < 3u; k = k + 1u) {
        for (var j = 0u; j < 3u; j = j + 1u) {
            for (var i = 0u; i < 3u; i = i + 1u) {
                let cell = water_grid_index(u32(base.x + i32(i)), u32(base.y + i32(j)), u32(base.z + i32(k)));
                let w = wx[i] * wy[j] * wz[k];
                let node = vec3<f32>(base) + vec3<f32>(f32(i), f32(j), f32(k));
                let d = node * WATER_H + WATER_ORIGIN - pos;
                let cd = vec3<f32>(dot(cx, d), dot(cy, d), dot(cz, d));

                var ok = true;
                let qmass = water_quantise(w * m, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmass != 0) {
                    // Checked fixed-point accumulation (S1 accumulate_checked
                    // as a CAS loop): the sum commits only when it cannot
                    // wrap; contention re-reads and retries, bounded. Retries
                    // exhausted stick the overflow bit and drop the
                    // contribution — the cell keeps its last representable
                    // value. Inlined because WGSL user-function pointer
                    // parameters must be function address space.
                    var cell_old = atomicLoad(&buf_out[cell * 4u + 3u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmass)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_out[cell * 4u + 3u], cell_old, cell_old + qmass);
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

                let t = w * m;
                let transfer = t * (vel + cd);
                var qmom = water_quantise(transfer.x, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmom != 0) {
                    var cell_old = atomicLoad(&buf_out[cell * 4u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmom)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_out[cell * 4u], cell_old, cell_old + qmom);
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
                qmom = water_quantise(transfer.y, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmom != 0) {
                    var cell_old = atomicLoad(&buf_out[cell * 4u + 1u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmom)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_out[cell * 4u + 1u], cell_old, cell_old + qmom);
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
                qmom = water_quantise(transfer.z, &ok);
                if (!ok) {
                    atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                    ok = true;
                } else if (qmom != 0) {
                    var cell_old = atomicLoad(&buf_out[cell * 4u + 2u]);
                    var settled = false;
                    for (var attempt = 0u; attempt < WATER_CAS_RETRIES; attempt = attempt + 1u) {
                        if (water_add_overflows(cell_old, qmom)) {
                            atomicOr(&buf_status_out[0], WATER_FAULT_INTEGER_OVERFLOW);
                            settled = true;
                            break;
                        }
                        let r = atomicCompareExchangeWeak(&buf_out[cell * 4u + 2u], cell_old, cell_old + qmom);
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
}
