// node.water_validate — HAND-AUTHORED standalone kernel.
//
// Global fault OR into the sticky status word (design step 7): every live
// candidate slot is checked for finiteness, positive mass, full stencil
// containment and the proof kinematic bounds (|v| <= 4 m/s, Frobenius |C|
// <= 64/s, 0 < rho <= 4*rho0). These are bounds, not clamps — exceeding them
// faults. Inactive slots (mass exactly zero) carry no checks.
//
// The output aliases the input status wire; the kernel ORs the incoming bits
// through (thread 0) so a pre-existing sticky fault survives validation, and
// ORs this pass's findings on top. When aliased, the load/Or on the same
// word is idempotent under concurrency — atomic ordering does not matter. A
// twelve-word sideband optionally records one internally consistent example
// of each kinematic violation; legacy one-word status buffers leave it disabled.
//
// Codegen gap (reported, S4): a single-global-word reduction (all threads
// contributing to one atomic word) is not a per-element body shape; the
// generated buffer wrapper cannot express it. Documented escape for
// atomic/global-dependency stages.

struct WaterParticle {
    position_mass: vec4<f32>,
    velocity_density: vec4<f32>,
    affine_x: vec4<f32>,
    affine_y: vec4<f32>,
    affine_z: vec4<f32>,
    previous_position: vec4<f32>,
}

struct Params {
    validate_count: u32,
    velocity_bound: f32,
    affine_bound: f32,
    density_max: f32,
    diagnostics_enabled: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> buf_particles: array<WaterParticle>;
@group(0) @binding(2) var<storage, read> buf_status: array<u32>;
@group(0) @binding(3) var<storage, read_write> buf_status_out: array<atomic<u32>>;

const WATER_DIAGNOSTIC_KINDS_WORD: u32 = 1u;
const WATER_VELOCITY_MAGNITUDE_WORD: u32 = 2u;
const WATER_VELOCITY_INDEX_WORD: u32 = 3u;
const WATER_VELOCITY_POSITION_X_WORD: u32 = 4u;
const WATER_VELOCITY_POSITION_Y_WORD: u32 = 5u;
const WATER_VELOCITY_POSITION_Z_WORD: u32 = 6u;
const WATER_AFFINE_MAGNITUDE_WORD: u32 = 7u;
const WATER_AFFINE_INDEX_WORD: u32 = 8u;
const WATER_AFFINE_POSITION_X_WORD: u32 = 9u;
const WATER_AFFINE_POSITION_Y_WORD: u32 = 10u;
const WATER_AFFINE_POSITION_Z_WORD: u32 = 11u;
const WATER_DIAGNOSTIC_KIND_VELOCITY: u32 = 1u;
const WATER_DIAGNOSTIC_KIND_AFFINE: u32 = 2u;

// Indices use idx + 1, leaving zero as the empty claim. Each kind has its own
// claim word so simultaneous velocity and affine failures are both visible.
fn record_kinematics_diagnostic(idx: u32, kind: u32, magnitude: f32, pos: vec3<f32>) {
    if (params.diagnostics_enabled == 0u) {
        return;
    }
    atomicOr(&buf_status_out[WATER_DIAGNOSTIC_KINDS_WORD], kind);
    var index_word = WATER_VELOCITY_INDEX_WORD;
    var magnitude_word = WATER_VELOCITY_MAGNITUDE_WORD;
    var position_word = WATER_VELOCITY_POSITION_X_WORD;
    if (kind == WATER_DIAGNOSTIC_KIND_AFFINE) {
        index_word = WATER_AFFINE_INDEX_WORD;
        magnitude_word = WATER_AFFINE_MAGNITUDE_WORD;
        position_word = WATER_AFFINE_POSITION_X_WORD;
    }
    loop {
        let claimed = atomicCompareExchangeWeak(
            &buf_status_out[index_word],
            0u,
            idx + 1u,
        );
        if (claimed.exchanged) {
            atomicStore(&buf_status_out[magnitude_word], bitcast<u32>(magnitude));
            atomicStore(&buf_status_out[position_word], bitcast<u32>(pos.x));
            atomicStore(&buf_status_out[position_word + 1u], bitcast<u32>(pos.y));
            atomicStore(&buf_status_out[position_word + 2u], bitcast<u32>(pos.z));
            return;
        }
        if (claimed.old_value != 0u) {
            return;
        }
    }
}

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= params.validate_count) {
        return;
    }
    if (idx == 0u) {
        // Sticky pass-through of the incoming status word. Idempotent when
        // input and output alias the same buffer.
        atomicOr(&buf_status_out[0], buf_status[0]);
    }
    let p = buf_particles[idx];
    let m = p.position_mass.w;
    if (m == 0.0) {
        return; // inactive slot
    }
    var bits = 0u;
    let pos = p.position_mass.xyz;
    let vel = p.velocity_density.xyz;
    let rho = p.velocity_density.w;
    let cx = p.affine_x.xyz;
    let cy = p.affine_y.xyz;
    let cz = p.affine_z.xyz;

    if (!water_finite3(pos) || !water_finite3(vel) || !water_finite1(m) || !water_finite1(rho)
        || !water_finite3(cx) || !water_finite3(cy) || !water_finite3(cz)) {
        bits = bits | WATER_FAULT_NONFINITE;
    } else {
        // Stencil containment: finite positions that leave the guard shell
        // fault instead of clipping.
        let q = (pos - WATER_ORIGIN) * WATER_INV_H;
        if (!water_q_plausible(q)) {
            bits = bits | WATER_FAULT_OUTSIDE_DOMAIN;
        } else {
            let base = water_stencil_base(q);
            if (!water_stencil_contained(base)) {
                bits = bits | WATER_FAULT_OUTSIDE_DOMAIN;
            }
        }
        let speed_sq = dot(vel, vel);
        if (!(speed_sq <= params.velocity_bound * params.velocity_bound)) {
            // NaN speed fails the comparison; finite excess too.
            bits = bits | WATER_FAULT_UNSUPPORTED_KINEMATICS;
            record_kinematics_diagnostic(
                idx,
                WATER_DIAGNOSTIC_KIND_VELOCITY,
                sqrt(speed_sq),
                pos,
            );
        }
        // Frobenius |C|^2 over the nine affine components — compare squared
        // against the squared bound to avoid a sqrt.
        let c_sq = dot(cx, cx) + dot(cy, cy) + dot(cz, cz);
        if (!(c_sq <= params.affine_bound * params.affine_bound)) {
            bits = bits | WATER_FAULT_UNSUPPORTED_KINEMATICS;
            record_kinematics_diagnostic(
                idx,
                WATER_DIAGNOSTIC_KIND_AFFINE,
                sqrt(c_sq),
                pos,
            );
        }
        if (!(rho > 0.0) || !(rho <= params.density_max)) {
            bits = bits | WATER_FAULT_INVALID_DENSITY;
        }
    }
    if (bits != 0u) {
        atomicOr(&buf_status_out[0], bits);
    }
}
