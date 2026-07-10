// node.remove_drift_3d — subtract the mean force over live particles from
// every per-particle force, so the internal field forces sum to zero and
// the fluid stops riding a net tide into a wall (BUG-066).
//
// The continuum physics says internal density-gradient forces sum to zero
// (momentum conservation); a discrete grid only approximates that, and the
// FluidSim3D feedback loop amplifies the ~0.5%-of-peak residue into a slow
// corner drift. This node enforces the conservation the math promises.
//
// Three passes (dispatched by the primitive with barriers between):
//   partial_main  — NUM_PARTIALS workgroups, grid-stride: per-workgroup
//                   partial sums of live-particle forces + live count.
//   finalize_main — one workgroup reduces the partials; writes
//                   partials[0] = vec4(mean.xyz, live_count).
//   apply_main    — out[i] = in[i] − mean × amount for i < active_count.
//
// The mean is over particles with life > 0 only; the subtraction is
// unconditional (entries past the live set are uninitialised by the force
// buffer convention). Summation order is a fixed tree over a fixed stride,
// so the result is bit-deterministic across runs (export determinism).

struct Uniforms {
    active_count: u32,
    num_partials: u32,
    amount: f32,
    _pad0: u32,
};

struct Particle {
    position: vec3<f32>,
    velocity: vec3<f32>,
    life: f32,
    age: f32,
    color: vec4<f32>,
};

// Packed 3-float force element (stride 12, matches Array<[f32; 3]>).
struct ForceVec {
    x: f32,
    y: f32,
    z: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> in_forces: array<ForceVec>;
@group(0) @binding(2) var<storage, read> particles: array<Particle>;
@group(0) @binding(3) var<storage, read_write> partials: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> out_forces: array<ForceVec>;

var<workgroup> wg_sum: array<vec4<f32>, 256>;

@compute @workgroup_size(256, 1, 1)
fn partial_main(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    var acc = vec4<f32>(0.0);
    let stride = 256u * u.num_partials;
    for (var i = wg.x * 256u + li; i < u.active_count; i += stride) {
        if particles[i].life > 0.0 {
            let f = in_forces[i];
            acc += vec4<f32>(f.x, f.y, f.z, 1.0);
        }
    }
    wg_sum[li] = acc;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if li < s {
            wg_sum[li] += wg_sum[li + s];
        }
        workgroupBarrier();
    }
    if li == 0u {
        partials[wg.x] = wg_sum[0];
    }
}

@compute @workgroup_size(256, 1, 1)
fn finalize_main(@builtin(local_invocation_index) li: u32) {
    var acc = vec4<f32>(0.0);
    for (var i = li; i < u.num_partials; i += 256u) {
        acc += partials[i];
    }
    wg_sum[li] = acc;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s = s >> 1u) {
        if li < s {
            wg_sum[li] += wg_sum[li + s];
        }
        workgroupBarrier();
    }
    if li == 0u {
        let t = wg_sum[0];
        let n = max(t.w, 1.0);
        partials[0] = vec4<f32>(t.xyz / n, t.w);
    }
}

@compute @workgroup_size(256, 1, 1)
fn apply_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if i >= u.active_count {
        return;
    }
    let m = partials[0].xyz * u.amount;
    let f = in_forces[i];
    out_forces[i] = ForceVec(f.x - m.x, f.y - m.y, f.z - m.z);
}
