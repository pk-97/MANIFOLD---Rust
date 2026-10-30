// GPU FLIP's dynamic bodies inside the pressure solve.
//
// Faces and cells are laid out as in gpu_flip_step/common.wgsl. A face record's
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
// The impulse and the body product run over the pressure solve's fine
// active slots: the solve's level-0 flags, lists and live count bound at
// 12–14, read exactly as gpu_flip_pressure.wgsl reads them (the lists hold
// pool slots, the flags are indexed by slot). A face a body owns contributes
// only beside water, and every cell beside water lies in an active slot.
// Partials are two a slot, indexed by slot; the finalize reads an inactive
// slot's as 0, which a run over every slot computes as +0.0, so sparse and
// full are bitwise. The velocity change runs over every held slot (binding
// 18), as a record lives with its cell.
//
// Fixed slots and a fixed reduction tree, so every result is identical run
// to run.
//
// Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis
// Fassbaender; see THIRD_PARTY_NOTICES.md): rigidpressurecoupling.h (the
// body product J M⁻¹ Jᵀ and the captured impulse),
// rigidboundaryvelocity.cpp (the pressure entries −h²·C·basis and the
// velocity change added to the solid velocity), rigidfluidcoupling.cpp (the
// order: solve, impulse, velocity change, then the constraint).

struct Params {
    // The lattice origin, world minus half a cell on every axis.
    origin: vec3<f32>,
    cell_size: f32,
    // The liquid's density, kg/m³.
    density: f32,
    // The pose time into the tick, as the step's solid passes take it.
    tick_seconds: f32,
    // Partial slots per body: two a pool slot.
    slots: u32,
    // This tick's first body row in `bodies`.
    first: u32,
    body_count: u32,
    // impulse_finalize: 1 adds the impulses into `reaction`.
    accumulate: u32,
    // impulse_finalize inside a round: the gate's solve-live triple
    // (`armed` is the gate there); 0 outside rounds, where it always runs.
    live: u32,
    // The tile pool's capacity P (world_tiles_read.wgsl ts_pool_tiles); H is
    // the table's own.
    pool_tiles: u32,
    _pad_h: u32,
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

struct ClockPlan {
    step_dt: f32,
    elapsed: f32,
    remaining: f32,
    maximum_speed: f32,
    cap_hit: u32,
    nonfinite: u32,
    step_index: u32,
    event: u32,
    numerical_end: f32,
    marker_limit: f32,
    _pad0: u32,
    live_mode: u32,
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
// The pressure solve's fine level, at word 0 of each: the gate triples
// (word 0 the live workgroup count), the slot flags and the active slot list.
@group(0) @binding(12) var<storage, read> armed: array<u32>;
@group(0) @binding(13) var<storage, read> flags: array<u32>;
@group(0) @binding(14) var<storage, read> lists: array<u32>;
@group(0) @binding(15) var<storage, read> clock_plan: array<ClockPlan>;
// Six vec4 per row of `bodies`: the body's packed 6 × 6 response to an
// impulse while it stays on its supports.
@group(0) @binding(16) var<storage, read> mobility: array<vec4<f32>>;
// The world tile table (world_tiles_read.wgsl): `water`, `open`, `solid`, `x`,
// `product` and `solid_rw` are pooled fields.
@group(0) @binding(17) var<storage, read> ts_table: array<u32>;
// Every held slot, for the pass over all records (velocity_change).
@group(0) @binding(18) var<storage, read> held: array<u32>;

// Entry (i, j) of body b's packed mobility.
fn mobility_at(b: u32, i: u32, j: u32) -> f32 {
    let lo = min(i, j);
    let hi = max(i, j);
    let k = lo * (13u - lo) / 2u + hi - lo;
    return mobility[6u * (u.first + b) + k / 4u][k % 4u];
}

const THREADS: u32 = 256u;

var<workgroup> scratch: array<array<f32, 256>, 6>;

fn ts_pool_tiles() -> u32 {
    return u.pool_tiles;
}


fn ts_table_word(i: u32) -> u32 {
    return ts_table[i];
}

// Whether thread gid's workgroup is inside the fine level's active list; a
// workgroup past it returns before any barrier, whole.
fn listed(gid: u32) -> bool {
    return (gid >> 8u) < armed[0];
}

// Thread gid's partial slot: its pool slot's two, by workgroup.
fn listed_partial(gid: u32) -> u32 {
    return 2u * lists[gid >> 9u] + ((gid >> 8u) & 1u);
}

fn clock_active() -> bool {
    return clock_plan[0].live_mode == 0u || clock_plan[0].step_dt > 0.0;
}

// The body owning axis a's face, or −1.
fn solid_owner(code: f32, a: i32) -> i32 {
    return i32((u32(max(code, 0.0)) >> (8u * u32(a))) & 255u) - 1;
}

// Axis a's face of the record at p: the cell centre moved half a cell down
// along a.
fn face_centre(p: vec3<i32>, a: i32) -> vec3<f32> {
    var c = u.origin + (vec3<f32>(p) + vec3<f32>(0.5)) * u.cell_size;
    c[a] = u.origin[a] + f32(p[a]) * u.cell_size;
    return c;
}

// Body b's centre of mass at this step's end. The step binds its posed rows
// (gpu_flip_step/solids.wgsl pose_bodies), the pose the solid distance takes.
fn body_centre(b: u32) -> vec3<f32> {
    return bodies[u.first + b].position_inv_mass.xyz;
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
fn wet_x(q: vec3<i32>) -> f32 {
    let cell = ts_cell_at(q);
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
fn face_impulse(p: vec3<i32>, a: i32) -> f32 {
    let at = ts_face(p);
    let w = open[at].face_weight[a];
    var lo = p;
    lo[a] = p[a] - 1;
    let h = u.cell_size;
    let c_hi = open[at].face_weight.w;
    let c_lo = open[ts_face(lo)].face_weight.w;
    return u.density * h * h * ((c_lo - w) * wet_x(lo) - (c_hi - w) * wet_x(p));
}

// Workgroup (g, b): body b's linear and angular impulse over the three low
// faces it owns of each cell in half g of the fine level's active list,
// tree-reduced into partials[(b · slots + listed_partial) · 8 ..]. A
// workgroup past the live count returns whole. A body that takes no reaction
// sums 0. Every face lies on the cell lattice as some cell's low face, and
// one a body owns pushes only beside water, inside the active slots.
@compute @workgroup_size(256, 1, 1)
fn impulse_partial(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    if !clock_active() {
        return;
    }
    let gid = wg.x * THREADS + li;
    if !listed(gid) {
        return;
    }
    let b = wg.y;
    let idx = ts_list_cell(lists[gid >> 9u], gid);
    var linear = vec3<f32>(0.0);
    var angular = vec3<f32>(0.0);
    if b < u.body_count && dynamic_body(b) {
        ts_home(idx);
        let c = body_centre(b);
        let p = ts_cell_coord(idx);
        let e = ts_face(p);
        let code = solid[e].face_velocity.w;
        for (var a = 0; a < 3; a = a + 1) {
            if solid_owner(code, a) != i32(b) {
                continue;
            }
            var push = vec3<f32>(0.0);
            push[a] = face_impulse(p, a);
            linear = linear + push;
            angular = angular + cross(face_centre(p, a) - c, push);
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
        partials[(b * u.slots + listed_partial(gid)) * 8u + li] = scratch[li][0];
    }
}

// One workgroup per body slot b: every partial of the fine level, in slot
// order, an inactive slot's read as 0, tree-reduced into the sums record;
// the velocity change M_c·impulse for a body that takes a reaction
// (rigidpressurecoupling.h Body::response, M_c the mobility held on the
// body's supports, which is symmetric, so the operator stays so), 0
// otherwise; with `accumulate`,
// the impulses added into the reaction. A slot past body_count is zero.
@compute @workgroup_size(256, 1, 1)
fn impulse_finalize(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    if !clock_active() {
        return;
    }
    // A round run after the solve stopped: uniform, before any barrier.
    if u.live != 0u && armed[3u * u.live] == 0u {
        return;
    }
    let b = wg.x;
    var mine = array<f32, 6>();
    if b < u.body_count {
        for (var s = li; s < u.slots; s = s + THREADS) {
            if flags[s >> 1u] == 0u {
                continue;
            }
            for (var k = 0u; k < 6u; k = k + 1u) {
                mine[k] = mine[k] + partials[(b * u.slots + s) * 8u + k];
            }
        }
    }
    for (var k = 0u; k < 6u; k = k + 1u) {
        scratch[k][li] = mine[k];
    }
    workgroupBarrier();
    for (var width = THREADS / 2u; width > 0u; width = width >> 1u) {
        if li < width {
            for (var k = 0u; k < 6u; k = k + 1u) {
                scratch[k][li] = scratch[k][li] + scratch[k][li + width];
            }
        }
        workgroupBarrier();
    }
    if li != 0u {
        return;
    }
    var total = array<f32, 16>();
    if b < u.body_count {
        for (var k = 0u; k < 6u; k = k + 1u) {
            total[k + k / 3u] = scratch[k][0];
        }
        if dynamic_body(b) {
            var impulse = array<f32, 6>(total[0], total[1], total[2], total[4], total[5], total[6]);
            for (var i = 0u; i < 6u; i = i + 1u) {
                var change = 0.0;
                for (var j = 0u; j < 6u; j = j + 1u) {
                    change = change + mobility_at(b, i, j) * impulse[j];
                }
                total[8u + i + i / 3u] = change;
            }
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

// One thread per cell of the fine level's active list, in place on
// `product`: in a water cell, plus (1/h)·Σ over its six owned faces of
// sign·(c − w)·(dv + dω × r)[a], sign +1 on the cell's high face and −1 on
// its low, c the cell's open volume, w the face's open fraction, r the face
// centre less the posed centre of mass, (dv, dω) the owner's sums. With the
// sums of the same vector's pressure impulse this is ρh·G M⁻¹ Gᵀ x, the
// bodies' share of the coupled operator (rigidpressurecoupling.h
// addMatrixProduct). Every water cell is in an active slot.
@compute @workgroup_size(256)
fn body_product(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() {
        return;
    }
    if !listed(gid.x) {
        return;
    }
    let idx = ts_list_cell(lists[gid.x >> 9u], gid.x);
    if !(water[idx] > 0.5) {
        return;
    }
    ts_home(idx);
    let p = ts_cell_coord(idx);
    let c = open[ts_face(p)].face_weight.w;
    var total = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        for (var side = 0; side < 2; side = side + 1) {
            var f = p;
            f[a] = p[a] + side;
            let at = ts_face(f);
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

// One thread per cell of every held slot, in place on `solid_rw` (a record
// lives with its cell): every face a body owns gains its owner's velocity
// change at the face centre (RigidBoundaryVelocityMap::addVelocityChange), so
// the constraint after it gives the water the body's velocity after the push.
@compute @workgroup_size(256)
fn velocity_change(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() {
        return;
    }
    let idx = ts_list_cell(held[gid.x >> 9u], gid.x);
    ts_home(idx);
    let p = ts_face_coord(idx);
    var out = solid_rw[idx];
    var changed = false;
    for (var a = 0; a < 3; a = a + 1) {
        let owner = solid_owner(out.face_velocity.w, a);
        if owner < 0 || u32(owner) >= u.body_count {
            continue;
        }
        let b = u32(owner);
        let r = face_centre(p, a) - body_centre(b);
        out.face_velocity[a] = out.face_velocity[a] + basis_dot(a, r, sums_dv(b), sums_dw(b));
        changed = true;
    }
    if changed {
        solid_rw[idx] = out;
    }
}

// Test-only: NaN into every partial of every body slot before an impulse, so
// a finalize that reads a slot the partial pass did not write, or an
// inactive slot's, shows in the sums. Never named outside the proofs.
@compute @workgroup_size(256)
fn poison_partials(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x < u.body_count * u.slots * 8u {
        partials[gid.x] = bitcast<f32>(0x7fc00000u);
    }
}
