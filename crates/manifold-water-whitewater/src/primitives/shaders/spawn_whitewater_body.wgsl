// node.spawn_whitewater — fusable BUFFER body, GATHER only. One thread per
// spawn slot j, FLIP's _emitDiffuseParticles (diffuseparticlesimulation.cpp
// :1912) for one new particle:
//   the frame emits total = offsets[emitters − 1]; slot j < min(total,
//   capacity) takes emission m = j, or ⌊j · total / capacity⌋ past capacity
//   (a uniform subset), and m's emitter e is the first with offsets[e] > m;
//   the particle lands in a cylinder about e's velocity: radius 8 marker
//   radii · √Xr, angle 2π·Xt, height Xh · |v| · dt along it (export supplies 1/60 s);
//   it is dropped outside the grid, or where the solid lattice's distance is
//   under a quarter cell;
//   lifetime = min + Ie·(max − min) ± variance, dropped at or below 0;
//   velocity is FLIP's MAC trilinear of the faces at its position.
// Dropped and unused slots write lifetime 0. Kind is left 0 for
// node.whitewater_type. Xr, Xt, Xh and the variance draw hash (j, seed,
// epoch) on their own streams.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's emitter radius over the cell size: 8 marker radii, a marker being
// the sphere of an eighth of a cell, (3 / 32π)^(1/3).
const SW_EMITTER_RADIUS: f32 = 2.481402;
// FLIP's _solidBufferWidth, cells.
const SW_SOLID_BUFFER: f32 = 0.25;
const SW_TWO_PI: f32 = 6.28318;

// floor(a·b / c) for a < c, through the 64-bit product.
fn sw_mul_div(a: u32, b: u32, c: u32) -> u32 {
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
    let lo = (p00 & 0xffffu) | (mid << 16u);
    var r = a1 * b1 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u);
    var q = 0u;
    for (var bit = 31; bit >= 0; bit = bit - 1) {
        let carry = r >> 31u;
        r = (r << 1u) | ((lo >> u32(bit)) & 1u);
        q = q << 1u;
        if carry == 1u || r >= c {
            r = r - c;
            q = q | 1u;
        }
    }
    return q;
}

// The first of the `emitters` running totals past emission m.
fn sw_emitter(m: u32, emitters: u32) -> u32 {
    var lo = 0u;
    var hi = emitters;
    while lo < hi {
        let mid = lo + (hi - lo) / 2u;
        if buf_offsets[mid] > m {
            hi = mid;
        } else {
            lo = mid + 1u;
        }
    }
    return lo;
}

fn sw_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn sw_face(axis: u32, i: u32) -> f32 {
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

// FLIP's MAC trilinear at q (whitewater cells), a face past the face grid
// reading 0; q lies inside the grid.
fn sw_velocity(q: vec3<f32>, cells: vec3<u32>, face_cells: vec3<u32>) -> vec3<f32> {
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = sw_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * sw_face(axis, i);
            }
        }
        v[axis] = sum;
    }
    return v;
}

// The solid lattice's distance at q, trilinear over its nodes (a node past
// the lattice reads 0), metres.
fn sw_solid(q: vec3<f32>, nodes: vec3<u32>) -> f32 {
    let lower = floor(q);
    let f = q - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let n = base + vec3<i32>(ww_corner(corner));
        if all(n >= vec3<i32>(0)) && all(n < vec3<i32>(nodes)) {
            let u = vec3<u32>(n);
            d = d + ww_corner_weight(f, corner) * buf_solid[u.x + nodes.x * (u.y + nodes.y * u.z)];
        }
    }
    return d;
}

fn body(
    idx: u32,
    count: u32,
    capacity: f32,
    emitters: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    seed: f32,
    epoch: f32,
    min_lifetime: f32,
    max_lifetime: f32,
    lifetime_variance: f32,
    dt: f32,
) -> Element2 {
    let empty = Element2(vec4<f32>(0.0), vec3<f32>(0.0), 0u);
    let n = min(u32(max(round(emitters), 0.0)), arrayLength(&buf_offsets));
    let slots = u32(max(round(capacity), 0.0));
    if n == 0u || slots == 0u {
        return empty;
    }
    let total = buf_offsets[n - 1u];
    if idx >= min(total, slots) {
        return empty;
    }
    var m = idx;
    if total > slots {
        m = sw_mul_div(idx, total, slots);
    }
    let e = sw_emitter(m, n);
    if e >= n || e >= arrayLength(&buf_particles) || e >= arrayLength(&buf_energy) {
        return empty;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) || nodes.x * nodes.y * nodes.z > arrayLength(&buf_solid) {
        return empty;
    }
    let cells = nodes - vec3<u32>(1u);
    if any(face_cells > cells) {
        return empty;
    }
    let emitter = buf_particles[e];
    let v = emitter.velocity;
    if !(emitter.position_radius.w > 0.0) || length(v) < 1e-3 {
        return empty;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let axis = normalize(v);
    // FLIP's test as written; its first clause always holds.
    var e1: vec3<f32>;
    if abs(axis.x) - 1.0 < 1e-3 && abs(axis.y) < 1e-3 && abs(axis.z) < 1e-3 {
        e1 = normalize(cross(axis, vec3<f32>(0.0, 1.0, 0.0)));
    } else {
        e1 = normalize(cross(axis, vec3<f32>(1.0, 0.0, 0.0)));
    }
    let e2 = normalize(cross(axis, e1));
    let s = bitcast<u32>(seed);
    let generation = u32(max(round(epoch), 0.0));
    let r = SW_EMITTER_RADIUS * h * sqrt(ww_random(idx, s, generation, 4u));
    let theta = ww_random(idx, s, generation, 5u) * SW_TWO_PI;
    let along = ww_random(idx, s, generation, 6u) * length(dt * v);
    let p = emitter.position_radius.xyz + r * cos(theta) * e1 + r * sin(theta) * e2 + along * axis;
    let q = ww_grid_position(p, vec3<f32>(center_x, center_y, center_z), size, cells);
    if !ww_in_grid(vec3<i32>(floor(q)), cells) {
        return empty;
    }
    if sw_solid(q, nodes) < SW_SOLID_BUFFER * h {
        return empty;
    }
    var lifetime = min_lifetime + buf_energy[e] * (max_lifetime - min_lifetime);
    lifetime = lifetime + lifetime_variance * (2.0 * ww_random(idx, s, generation, 7u) - 1.0);
    if !(lifetime > 0.0) {
        return empty;
    }
    return Element2(vec4<f32>(p, lifetime), sw_velocity(q, cells, face_cells), 0u);
}
