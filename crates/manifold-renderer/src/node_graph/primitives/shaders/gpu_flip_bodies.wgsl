// GPU FLIP's dynamic bodies inside the pressure solve (gpu_flip_bodies.rs,
// docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in the water),
// LIQUID_SOLVER_SEAM_DESIGN.md D7 (bodies inside the pressure solve)).
//
// Faces and cells are laid out as in gpu_flip_step.wgsl. A face record's
// three faces each name the body that owns them in the solid face velocity's
// w (the step's solid_face_velocity pass): Σ over the axes a of
// (b_a + 1) · 256^a, b_a the body (0 to body_count − 1) or −1 for none.
// Exact in f32 for 64 bodies.
//
// A body's sums record, 16 floats per body: linear impulse (N·s), angular
// impulse about the posed centre of mass (N·m·s), velocity change (m/s),
// angular velocity change (rad/s), each a vec4 with w = 0. The reaction is
// 8 floats per body: linear, then angular impulse, each with w = 0.
//
// Fixed stride and a fixed reduction tree, so every result is identical run
// to run.
//
// Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis
// Fassbaender; see THIRD_PARTY_NOTICES.md): rigidpressurecoupling.h (the
// body product J M⁻¹ Jᵀ and the captured impulse),
// rigidboundaryvelocity.cpp (the pressure entries −h²·C·basis and the
// velocity change added to the solid velocity), rigidfluidcoupling.cpp (the
// order: solve, impulse, velocity change, then the constraint).

struct Params {
    n: vec3<u32>,
    // Workgroups per body in the partial pass.
    groups: u32,
    lattice_min: vec3<f32>,
    cell_size: f32,
    // The liquid's density, kg/m³.
    density: f32,
    // The pose time into the tick, as the step's solid passes take it.
    tick_seconds: f32,
    // This tick's first body row in `bodies`.
    first: u32,
    body_count: u32,
    // impulse_finalize: 1 adds the impulses into `reaction`.
    accumulate: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

struct FaceSample {
    face_velocity: vec4<f32>,
    face_weight: vec4<f32>,
};

struct LiquidBody {
    position_inv_mass: vec4<f32>,
    rotation: vec4<f32>,
    linear_velocity: vec4<f32>,
    angular_velocity: vec4<f32>,
    inv_inertia_x: vec4<f32>,
    inv_inertia_y: vec4<f32>,
    inv_inertia_z: vec4<f32>,
    accel_shape: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> water: array<f32>;
@group(0) @binding(2) var<storage, read> open: array<FaceSample>;
@group(0) @binding(3) var<storage, read> solid: array<FaceSample>;
@group(0) @binding(4) var<storage, read> bodies: array<LiquidBody>;
@group(0) @binding(5) var<storage, read> x: array<f32>;
@group(0) @binding(7) var<storage, read_write> partials: array<f32>;
@group(0) @binding(8) var<storage, read_write> sums: array<f32>;
@group(0) @binding(9) var<storage, read_write> reaction: array<f32>;
@group(0) @binding(10) var<storage, read_write> product: array<f32>;
@group(0) @binding(11) var<storage, read_write> solid_rw: array<FaceSample>;

const THREADS: u32 = 256u;

var<workgroup> scratch: array<array<f32, 256>, 6>;

fn lattice() -> vec3<i32> {
    return vec3<i32>(u.n);
}

fn unflatten(idx: u32, m: vec3<i32>) -> vec3<i32> {
    return vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
}

fn flatten(p: vec3<i32>, m: vec3<i32>) -> u32 {
    return u32(p.x + m.x * (p.y + m.y * p.z));
}

// The body owning axis a's face, or −1.
fn solid_owner(code: f32, a: i32) -> i32 {
    return i32((u32(max(code, 0.0)) >> (8u * u32(a))) & 255u) - 1;
}

// Axis a's face of the record at p: the cell centre moved half a cell down
// along a.
fn face_centre(p: vec3<i32>, a: i32) -> vec3<f32> {
    var c = u.lattice_min + (vec3<f32>(p) + vec3<f32>(0.5)) * u.cell_size;
    c[a] = u.lattice_min[a] + f32(p[a]) * u.cell_size;
    return c;
}

// Body b's centre of mass posed tick_seconds into the tick, as the solid
// distance poses it.
fn body_centre(b: u32) -> vec3<f32> {
    let bd = bodies[u.first + b];
    return bd.position_inv_mass.xyz + bd.linear_velocity.xyz * u.tick_seconds;
}

// A body that takes a reaction: finite mass and a shape
// (liquid::coupling::takes_reaction).
fn dynamic_body(b: u32) -> bool {
    let bd = bodies[u.first + b];
    return bd.position_inv_mass.w > 0.0 && bd.accel_shape.w >= 0.0;
}

// The face's velocity along a from a velocity change (dv, dω) at r from the
// centre of mass: the engine's rigid boundary basis, (dv + dω × r)[a].
fn basis_dot(a: i32, r: vec3<f32>, dv: vec3<f32>, dw: vec3<f32>) -> f32 {
    return (dv + cross(dw, r))[a];
}

fn sums_dv(b: u32) -> vec3<f32> {
    let s = 16u * b;
    return vec3<f32>(sums[s + 8u], sums[s + 9u], sums[s + 10u]);
}

fn sums_dw(b: u32) -> vec3<f32> {
    let s = 16u * b;
    return vec3<f32>(sums[s + 12u], sums[s + 13u], sums[s + 14u]);
}

// x in a water cell, 0 elsewhere.
fn wet_x(q: vec3<i32>, n: vec3<i32>) -> f32 {
    let cell = flatten(q, n);
    return select(0.0, x[cell], water[cell] > 0.5);
}

// The pressure's impulse along a through inner face a of record p, owned by
// a body: ρh²·((c_lo − w)·x_lo − (c_hi − w)·x_hi), w the face's open
// fraction, c each side's open volume and x each side's pressure in a water
// cell (the engine's forcePerPressure, −h²·C·basis with C = w − c, times the
// pressure; ours is dt·P/ρ). The pressure is the body's only reaction, as in
// the engine (rigidfluidcoupling.cpp): the solid constraint's friction acts
// on the water alone. An explicit friction reaction diverges once
// ρ·h·f·A_wet/m passes 2, which any light body does.
fn face_impulse(p: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> f32 {
    let at = flatten(p, m);
    let w = open[at].face_weight[a];
    var lo = p;
    lo[a] = p[a] - 1;
    let h = u.cell_size;
    let c_hi = open[at].face_weight.w;
    let c_lo = open[flatten(lo, m)].face_weight.w;
    return u.density * h * h * ((c_lo - w) * wet_x(lo, n) - (c_hi - w) * wet_x(p, n));
}

// Workgroup (g, b): body b's linear and angular impulse over every inner
// face it owns, grid-strided from record g · 256, tree-reduced into
// partials[(b · groups + g) · 8 ..]. A body that takes no reaction sums 0.
@compute @workgroup_size(256, 1, 1)
fn impulse_partial(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let b = wg.y;
    let n = lattice();
    let m = n + vec3<i32>(1);
    let total = u32(m.x) * u32(m.y) * u32(m.z);
    var linear = vec3<f32>(0.0);
    var angular = vec3<f32>(0.0);
    if b < u.body_count && dynamic_body(b) {
        let c = body_centre(b);
        for (var e = wg.x * THREADS + li; e < total; e = e + u.groups * THREADS) {
            let p = unflatten(e, m);
            let code = solid[e].face_velocity.w;
            for (var a = 0; a < 3; a = a + 1) {
                var other = p;
                other[a] = 0;
                if !all(other < n) || p[a] == 0 || p[a] == n[a] || solid_owner(code, a) != i32(b) {
                    continue;
                }
                var push = vec3<f32>(0.0);
                push[a] = face_impulse(p, a, n, m);
                linear = linear + push;
                angular = angular + cross(face_centre(p, a) - c, push);
            }
        }
    }
    scratch[0][li] = linear.x;
    scratch[1][li] = linear.y;
    scratch[2][li] = linear.z;
    scratch[3][li] = angular.x;
    scratch[4][li] = angular.y;
    scratch[5][li] = angular.z;
    workgroupBarrier();
    for (var width = THREADS / 2u; width > 0u; width = width >> 1u) {
        if li < width {
            for (var k = 0u; k < 6u; k = k + 1u) {
                scratch[k][li] = scratch[k][li] + scratch[k][li + width];
            }
        }
        workgroupBarrier();
    }
    if li < 6u {
        partials[(b * u.groups + wg.x) * 8u + li] = scratch[li][0];
    }
}

// One thread per body slot: the partials added in group order into the
// sums record, the velocity change M⁻¹·impulse for a body that takes a
// reaction (rigidpressurecoupling.h Body::response), 0 otherwise; with
// `accumulate`, the impulses added into the reaction. Slots past body_count
// are zero.
@compute @workgroup_size(64, 1, 1)
fn impulse_finalize(@builtin(local_invocation_index) b: u32) {
    var total = array<f32, 16>();
    if b < u.body_count {
        for (var k = 0u; k < 6u; k = k + 1u) {
            var s = 0.0;
            for (var g = 0u; g < u.groups; g = g + 1u) {
                s = s + partials[(b * u.groups + g) * 8u + k];
            }
            total[k + k / 3u] = s;
        }
        if dynamic_body(b) {
            let bd = bodies[u.first + b];
            let l = vec3<f32>(total[4], total[5], total[6]);
            total[8] = bd.position_inv_mass.w * total[0];
            total[9] = bd.position_inv_mass.w * total[1];
            total[10] = bd.position_inv_mass.w * total[2];
            total[12] = dot(bd.inv_inertia_x.xyz, l);
            total[13] = dot(bd.inv_inertia_y.xyz, l);
            total[14] = dot(bd.inv_inertia_z.xyz, l);
        }
        if u.accumulate != 0u {
            for (var k = 0u; k < 8u; k = k + 1u) {
                reaction[8u * b + k] = reaction[8u * b + k] + total[k];
            }
        }
    }
    for (var k = 0u; k < 16u; k = k + 1u) {
        sums[16u * b + k] = total[k];
    }
}

// One thread per cell, in place on `product`: in a water cell, plus
// (1/h)·Σ over its six inner owned faces of sign·(c − w)·(dv + dω × r)[a],
// sign +1 on the cell's high face and −1 on its low, c the cell's open
// volume, w the face's open fraction, r the face centre less the posed
// centre of mass, (dv, dω) the owner's sums. With the sums of the same
// vector's pressure impulse this is ρh·G M⁻¹ Gᵀ x, the bodies' share of the
// coupled operator (rigidpressurecoupling.h addMatrixProduct).
@compute @workgroup_size(256)
fn body_product(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let n = lattice();
    if idx >= u32(n.x) * u32(n.y) * u32(n.z) || !(water[idx] > 0.5) {
        return;
    }
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    let c = open[flatten(p, m)].face_weight.w;
    var total = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var side = 0; side < 2; side = side + 1) {
            var f = p;
            f[a] = p[a] + side;
            if f[a] == 0 || f[a] == n[a] {
                continue;
            }
            let at = flatten(f, m);
            let owner = solid_owner(solid[at].face_velocity.w, a);
            if owner < 0 || u32(owner) >= u.body_count {
                continue;
            }
            let b = u32(owner);
            let r = face_centre(f, a) - body_centre(b);
            let sign = select(-1.0, 1.0, side == 1);
            total = total + sign * (c - open[at].face_weight[a]) * basis_dot(a, r, sums_dv(b), sums_dw(b));
        }
    }
    product[idx] = product[idx] + total / u.cell_size;
}

// One thread per face record, in place on `solid_rw`: every inner face a
// body owns gains its owner's velocity change at the face centre
// (RigidBoundaryVelocityMap::addVelocityChange), so the constraint after it
// gives the water the body's velocity after the push.
@compute @workgroup_size(256)
fn velocity_change(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let n = lattice();
    let m = n + vec3<i32>(1);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return;
    }
    let p = unflatten(idx, m);
    var out = solid_rw[idx];
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        let owner = solid_owner(out.face_velocity.w, a);
        if !all(other < n) || p[a] == 0 || p[a] == n[a] || owner < 0 || u32(owner) >= u.body_count {
            continue;
        }
        let b = u32(owner);
        let r = face_centre(p, a) - body_centre(b);
        out.face_velocity[a] = out.face_velocity[a] + basis_dot(a, r, sums_dv(b), sums_dw(b));
    }
    solid_rw[idx] = out;
}
