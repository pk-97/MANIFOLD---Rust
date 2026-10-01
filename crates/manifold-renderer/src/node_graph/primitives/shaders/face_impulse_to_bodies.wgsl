// Ported from FLIP Fluids rigidboundaryvelocity.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.
// node.face_impulse_to_bodies — each body's share of a face grid of
// impulses (node.pressure_face_impulse, node.friction_face_impulse): the
// linear impulse Σ s and the angular impulse Σ r × s·e_a about the body's
// centre of mass over every face it owns, r from the centre posed
// tick_seconds on to the face centre (RigidBoundaryVelocityMap's Jᵀ). Two
// passes with a barrier between:
//   partial_main  — workgroup (g, b): grid-stride over the face records,
//                   tree-reduced in workgroup memory to partials[b, g].
//   finalize_main — one thread per body adds its partials in order, adds
//                   `base`, and applies M⁻¹: out[16b ..] = linear impulse,
//                   angular impulse, dv, dω (each a vec4, w = 0). A body
//                   that takes no reaction (1/m ≤ 0 or no shape) gets no
//                   dv or dω; bodies past body_count are zero. Bodies
//                   below body_count also go into `reaction` when bound.
// Fixed stride and fixed tree, so the result is identical run to run.

struct Params {
    lattice_min: vec3<f32>,
    cell_size: f32,
    nodes: vec3<u32>,
    body_count: u32,
    first: u32,
    groups: u32,
    has_base: u32,
    has_reaction: u32,
    tick_seconds: f32,
    max_bodies: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> impulses: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> bodies: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> base: array<f32>;
@group(0) @binding(4) var<storage, read_write> partials: array<f32>;
@group(0) @binding(5) var<storage, read_write> out: array<f32>;
@group(0) @binding(6) var<storage, read_write> reaction: array<f32>;

var<workgroup> sums: array<array<f32, 256>, 6>;

fn owner(code: f32, a: u32) -> i32 {
    return i32((u32(max(code, 0.0)) >> (8u * a)) & 255u) - 1;
}

// The posed centre of mass of body b: position + linear velocity · t.
fn centre(b: u32) -> vec3<f32> {
    let row = 8u * (u.first + b);
    return bodies[row].xyz + bodies[row + 2u].xyz * u.tick_seconds;
}

@compute @workgroup_size(256, 1, 1)
fn partial_main(
    @builtin(local_invocation_index) li: u32,
    @builtin(workgroup_id) wg: vec3<u32>,
) {
    let b = wg.y;
    let m = u.nodes + vec3<u32>(1u);
    let faces = m.x * m.y * m.z;
    let c = centre(b);
    var linear = vec3<f32>(0.0);
    var angular = vec3<f32>(0.0);
    for (var e = wg.x * 256u + li; e < faces; e = e + u.groups * 256u) {
        // A face record is two vec4s: velocity, then weight.
        let v = impulses[2u * e];
        let p = vec3<u32>(e % m.x, (e / m.x) % m.y, e / (m.x * m.y));
        for (var a = 0u; a < 3u; a = a + 1u) {
            if owner(v.w, a) != i32(b) {
                continue;
            }
            var x = u.lattice_min + (vec3<f32>(p) + vec3<f32>(0.5)) * u.cell_size;
            x[a] = u.lattice_min[a] + f32(p[a]) * u.cell_size;
            var axis = vec3<f32>(0.0);
            axis[a] = 1.0;
            linear = linear + v[a] * axis;
            angular = angular + v[a] * cross(x - c, axis);
        }
    }
    sums[0][li] = linear.x;
    sums[1][li] = linear.y;
    sums[2][li] = linear.z;
    sums[3][li] = angular.x;
    sums[4][li] = angular.y;
    sums[5][li] = angular.z;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width = width >> 1u) {
        if li < width {
            for (var k = 0u; k < 6u; k = k + 1u) {
                sums[k][li] = sums[k][li] + sums[k][li + width];
            }
        }
        workgroupBarrier();
    }
    if li < 6u {
        partials[(b * u.groups + wg.x) * 8u + li] = sums[li][0];
    }
}

@compute @workgroup_size(64, 1, 1)
fn finalize_main(@builtin(local_invocation_index) b: u32) {
    if b >= u.max_bodies {
        return;
    }
    var total = array<f32, 16>();
    if b < u.body_count {
        for (var k = 0u; k < 6u; k = k + 1u) {
            var s = 0.0;
            for (var g = 0u; g < u.groups; g = g + 1u) {
                s = s + partials[(b * u.groups + g) * 8u + k];
            }
            if u.has_base != 0u {
                s = s + base[16u * b + k + k / 3u];
            }
            total[k + k / 3u] = s;
        }
        let row = 8u * (u.first + b);
        let inv_mass = bodies[row].w;
        if inv_mass > 0.0 && bodies[row + 7u].w >= 0.0 {
            let l = vec3<f32>(total[4], total[5], total[6]);
            for (var a = 0u; a < 3u; a = a + 1u) {
                total[8u + a] = inv_mass * total[a];
                total[12u + a] = dot(bodies[row + 4u + a].xyz, l);
            }
        }
    }
    for (var k = 0u; k < 16u; k = k + 1u) {
        out[16u * b + k] = total[k];
        if u.has_reaction != 0u && b < u.body_count {
            reaction[16u * b + k] = total[k];
        }
    }
}
