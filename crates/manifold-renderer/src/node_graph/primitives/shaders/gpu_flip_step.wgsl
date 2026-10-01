// GPU FLIP's water step (gpu_flip_step.rs, docs/GPU_FLIP_PRESSURE_SOLVE.md
// section 1 (the step)): every pass of one step but the sort, the solid
// distance and the pressure solve, each a separate entry point. Included
// after liquid_pose.wgsl, liquid_collider.wgsl and liquid_field.wgsl.
//
// Cells are n per axis from the box minimum, x fastest. A face grid is
// (n + 1)³ FaceSample records indexed like the cells with m = n + 1: record
// p holds the low x, y and z faces of cell p, face a existing when every
// other coordinate is under n. Face a of p sits at p on axis a and p + ½ on
// the other two, in cells from the box minimum. The solid corner lattice is
// (n + 1)³ values from the box minimum.
//
// The CPU sizes every buffer for the lattice and the particle slots before
// it dispatches; each pass only checks its thread is inside its range.
//
// Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis
// Fassbaender; see THIRD_PARTY_NOTICES.md): velocityadvector.cpp (particles
// to faces), particlelevelset.cpp (the particle distance),
// levelsetutils.cpp and meshlevelset.cpp (the solid open fractions),
// fluidsimulation.cpp (the solids' face velocity and the constraint) and
// pressuresolver.cpp (divergence and the pressure subtraction).

struct Params {
    // Cells per axis.
    n: vec3<u32>,
    // Slots of `sorted`.
    capacity: u32,
    box_min: vec3<f32>,
    cell_size: f32,
    gravity: vec3<f32>,
    step_dt: f32,
    field_nodes: vec3<u32>,
    field_spacing: f32,
    tick_index: i32,
    step_in_tick: i32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
    body_count: i32,
    // Body rows, at most what `bodies` holds.
    rows: i32,
    tick_seconds: f32,
    // The FLIP share for this step.
    flip: f32,
    // The farthest one RK3 stage moves, in cells.
    max_travel: f32,
    // Particles a full cell holds.
    rest: f32,
    // The density source's rate (1/s).
    rate: f32,
    // The largest |box_min| component, for the solid faces' tolerance.
    box_offset: f32,
    // subtract: 1 reads `phi` for the free surface, 0 keeps air at zero.
    ghost: u32,
    // Particles the move writes.
    particles: u32,
    // Records `shapes` holds.
    shapes_len: u32,
};

struct CellRange {
    start: u32,
    count: u32,
};

struct FluidParticle {
    position_radius: vec4<f32>,
    velocity: vec3<f32>,
    id: u32,
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

struct LiquidShape {
    origin_spacing: vec4<f32>,
    dims_x: u32,
    dims_y: u32,
    dims_z: u32,
    atlas_offset: u32,
    scale_min: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Params;
@group(0) @binding(1) var<storage, read> ranges: array<CellRange>;
@group(0) @binding(2) var<storage, read> sorted: array<FluidParticle>;
@group(0) @binding(3) var<storage, read> faces_in: array<FaceSample>;
@group(0) @binding(4) var<storage, read_write> faces_out: array<FaceSample>;
@group(0) @binding(5) var<storage, read_write> cell_out: array<f32>;
@group(0) @binding(6) var<storage, read> water: array<f32>;
@group(0) @binding(7) var<storage, read> phi: array<f32>;
@group(0) @binding(8) var<storage, read> pressure: array<f32>;
@group(0) @binding(9) var<storage, read> solid: array<f32>;
@group(0) @binding(10) var<storage, read> solid_faces: array<FaceSample>;
@group(0) @binding(11) var<storage, read> solid_velocity: array<FaceSample>;
@group(0) @binding(12) var<storage, read> forces: array<f32>;
@group(0) @binding(13) var<storage, read> impulses: array<f32>;
@group(0) @binding(14) var<storage, read> bodies: array<LiquidBody>;
@group(0) @binding(15) var<storage, read> shapes: array<LiquidShape>;
@group(0) @binding(16) var<storage, read> atlas: array<u32>;
@group(0) @binding(17) var<storage, read> old: array<FaceSample>;
@group(0) @binding(18) var<storage, read> advect: array<FaceSample>;
@group(0) @binding(19) var<storage, read_write> particles_out: array<FluidParticle>;
@group(0) @binding(20) var<storage, read_write> faces_rw: array<FaceSample>;

fn lattice() -> vec3<i32> {
    return vec3<i32>(u.n);
}

fn cell_total() -> u32 {
    return u.n.x * u.n.y * u.n.z;
}

fn face_total() -> u32 {
    let m = u.n + vec3<u32>(1u);
    return m.x * m.y * m.z;
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

// Face a of record p exists when every other coordinate is inside the cells.
fn face_exists(p: vec3<i32>, n: vec3<i32>, a: i32) -> bool {
    var other = p;
    other[a] = 0;
    return all(other < n);
}

// One thread per cell: 1 where the sort put a particle in it, else 0.
@compute @workgroup_size(256)
fn classify(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    cell_out[idx] = select(0.0, 1.0, ranges[idx].count > 0u);
}

// A box wall lets water leave and never enter: of the velocity on a wall
// face it keeps only the part pointing into the box (up from the floor face,
// index 0; down from the lid face, index n).
fn wall(v: f32, low: bool) -> f32 {
    return select(min(v, 0.0), max(v, 0.0), low);
}

// One thread per face record. Each face sums the engine's Wyvill weight
// 1 − (4/9)·s³/r⁶ + (17/9)·s²/r⁴ − (22/9)·s/r² for s = |q − face|² < r²,
// r = √3/2 cells, over every live particle in the 3 × 3 × 3 cells around p,
// and the weighted velocity along its normal. A face over weight 1e-6 gets
// the ratio; any other gets velocity 0 and weight 0 for the extension to
// fill. A box wall face keeps only the part leaving the wall and is always
// valid (weight 1), so the extension never writes it.
@compute @workgroup_size(256)
fn particles_to_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    var exists = vec3<bool>(false);
    for (var a = 0; a < 3; a = a + 1) {
        exists[a] = face_exists(p, n, a);
    }
    if !any(exists) {
        faces_out[idx] = out;
        return;
    }
    let inv_h = 1.0 / u.cell_size;
    let first = max(p - vec3<i32>(1), vec3<i32>(0));
    let last = min(p + vec3<i32>(1), n - vec3<i32>(1));
    let slots = u.capacity;
    let rsq = 0.75;
    let coef1 = (4.0 / 9.0) / (rsq * rsq * rsq);
    let coef2 = (17.0 / 9.0) / (rsq * rsq);
    let coef3 = (22.0 / 9.0) / rsq;
    var weight = vec3<f32>(0.0);
    var momentum = vec3<f32>(0.0);
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let range = ranges[flatten(vec3<i32>(x, y, z), n)];
                let start = min(range.start, slots);
                let end = start + min(range.count, slots - start);
                for (var s = start; s < end; s = s + 1u) {
                    let particle = sorted[s];
                    if !(particle.position_radius.w > 0.0) {
                        continue;
                    }
                    let q = (particle.position_radius.xyz - u.box_min) * inv_h;
                    for (var a = 0; a < 3; a = a + 1) {
                        if !exists[a] {
                            continue;
                        }
                        var face = vec3<f32>(p) + vec3<f32>(0.5);
                        face[a] = f32(p[a]);
                        let v = face - q;
                        let d2 = dot(v, v);
                        if !(d2 < rsq) {
                            continue;
                        }
                        let w = 1.0 - coef1 * d2 * d2 * d2 + coef2 * d2 * d2 - coef3 * d2;
                        weight[a] = weight[a] + w;
                        momentum[a] = momentum[a] + w * particle.velocity[a];
                    }
                }
            }
        }
    }
    let valid = weight > vec3<f32>(1e-6);
    var velocity = select(vec3<f32>(0.0), momentum / max(weight, vec3<f32>(1e-6)), valid);
    weight = select(vec3<f32>(0.0), weight, valid);
    for (var a = 0; a < 3; a = a + 1) {
        if exists[a] && (p[a] == 0 || p[a] == n[a]) {
            velocity[a] = wall(velocity[a], p[a] == 0);
            weight[a] = 1.0;
        }
    }
    out.face_velocity = vec4<f32>(velocity, 0.0);
    out.face_weight = vec4<f32>(weight, 0.0);
    faces_out[idx] = out;
}

// One thread per face record, `faces_in` to `faces_out`. A valid face
// (weight > 0) is copied. An invalid one takes the mean velocity of the
// valid faces of its component among its six grid neighbours and becomes
// valid (weight 1); with none it stays as it was.
@compute @workgroup_size(256)
fn extend_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let here = faces_in[idx];
    for (var a = 0; a < 3; a = a + 1) {
        // Faces of component a span 0..=n on axis a and 0..n on the others.
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        if any(p > top) {
            continue;
        }
        out.face_velocity[a] = here.face_velocity[a];
        out.face_weight[a] = here.face_weight[a];
        if here.face_weight[a] > 0.0 {
            continue;
        }
        var sum = 0.0;
        var hits = 0.0;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                if q[b] < 0 || q[b] > top[b] {
                    continue;
                }
                let neighbour = faces_in[flatten(q, m)];
                if neighbour.face_weight[a] > 0.0 {
                    sum = sum + neighbour.face_velocity[a];
                    hits = hits + 1.0;
                }
            }
        }
        if hits > 0.0 {
            out.face_velocity[a] = sum / hits;
            out.face_weight[a] = 1.0;
        }
    }
    faces_out[idx] = out;
}

fn gravity_force(x: vec3<f32>, origin: vec3<f32>, base: u32, axis: u32) -> f32 {
    var sum = 0.0;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = liquid_field_corner(x, origin, u.field_spacing, u.field_nodes, k);
        sum = fma(forces[base + c.index * 4u + axis], c.weight, sum);
    }
    return sum;
}

fn gravity_impulse(x: vec3<f32>, origin: vec3<f32>, axis: u32) -> f32 {
    var sum = 0.0;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = liquid_field_corner(x, origin, u.field_spacing, u.field_nodes, k);
        sum = fma(impulses[c.index * 4u + axis], c.weight, sum);
    }
    return sum;
}

// One thread per face record, `faces_in` to `faces_out`: each face gains
// step_dt · (g + forces(x)) along its normal a, x its centre, plus the
// impulses on step 0 of impulse_tick, read from the domain's coarse field
// lattices (origin the box minimum). A box wall face then keeps only the
// part leaving the wall. Weights pass through.
@compute @workgroup_size(256)
fn face_gravity(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    let here = faces_in[idx];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let origin = u.box_min;
    var force_base = 0u;
    if u.force_lattices > 0 {
        force_base = liquid_field_force_base(u.tick_index, u.first_tick, u.force_lattices, u.field_nodes);
    }
    let impulse = u.tick_index == u.impulse_tick && u.step_in_tick == 0;
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) {
            continue;
        }
        out.face_weight[a] = here.face_weight[a];
        var centre = vec3<f32>(p) + vec3<f32>(0.5);
        centre[a] = f32(p[a]);
        let x = fma(centre, vec3<f32>(u.cell_size), origin);
        var accel = u.gravity[a];
        if u.force_lattices > 0 {
            accel = accel + gravity_force(x, origin, force_base, u32(a));
        }
        var v = fma(accel, u.step_dt, here.face_velocity[a]);
        if impulse {
            v = v + gravity_impulse(x, origin, u32(a));
        }
        if p[a] == 0 {
            out.face_velocity[a] = max(v, 0.0);
        } else if p[a] == n[a] {
            out.face_velocity[a] = min(v, 0.0);
        } else {
            out.face_velocity[a] = v;
        }
    }
    faces_out[idx] = out;
}

// The tetrahedron fractions of LevelsetUtils, phi sorted ascending.
fn solid_tet(a: f32, b: f32, c: f32, d: f32) -> f32 {
    return a * a * a / ((a - b) * (a - c) * (a - d));
}

fn solid_prism(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let p = a / (a - c);
    let q = a / (a - d);
    let r = b / (b - d);
    let s = b / (b - c);
    return p * q * (1.0 - s) + q * (1.0 - r) * s + r * s;
}

// The fraction of a tetrahedron inside the solid, sorted as the engine's
// five-swap network sorts it.
fn solid_tet_inside(p0: f32, p1: f32, p2: f32, p3: f32) -> f32 {
    var a = p0;
    var b = p1;
    var c = p2;
    var d = p3;
    var t = 0.0;
    if a > b { t = a; a = b; b = t; }
    if c > d { t = c; c = d; d = t; }
    if a > c { t = a; a = c; c = t; }
    if b > d { t = b; b = d; d = t; }
    if b > c { t = b; b = c; c = t; }
    if d <= 0.0 {
        return 1.0;
    }
    if c <= 0.0 {
        return 1.0 - solid_tet(d, c, b, a);
    }
    if b <= 0.0 {
        return solid_prism(a, b, c, d);
    }
    if a <= 0.0 {
        return solid_tet(a, b, c, d);
    }
    return 0.0;
}

// The fraction of a cube inside the solid: the mean of its two
// five-tetrahedron splits (LevelsetUtils::volumeFraction), exactly 0 or 1
// when every corner agrees (MeshLevelSet::_getCellWeight). c[i + 2j + 4k] is
// phi at corner (i, j, k).
fn solid_cube_inside(c: array<f32, 8>) -> f32 {
    var all_in = true;
    var all_out = true;
    for (var i = 0; i < 8; i = i + 1) {
        all_in = all_in && c[i] < 0.0;
        all_out = all_out && c[i] >= 0.0;
    }
    if all_in {
        return 1.0;
    }
    if all_out {
        return 0.0;
    }
    let p000 = c[0];
    let p100 = c[1];
    let p010 = c[2];
    let p110 = c[3];
    let p001 = c[4];
    let p101 = c[5];
    let p011 = c[6];
    let p111 = c[7];
    return (solid_tet_inside(p000, p001, p101, p011)
        + solid_tet_inside(p000, p101, p100, p110)
        + solid_tet_inside(p000, p010, p011, p110)
        + solid_tet_inside(p101, p011, p111, p110)
        + 2.0 * solid_tet_inside(p000, p011, p101, p110)
        + solid_tet_inside(p100, p101, p001, p111)
        + solid_tet_inside(p100, p001, p000, p010)
        + solid_tet_inside(p100, p110, p111, p010)
        + solid_tet_inside(p001, p111, p011, p010)
        + 2.0 * solid_tet_inside(p100, p111, p001, p010)) / 12.0;
}

// The fraction of the segment from a to b inside the solid (phi < 0).
fn solid_segment(a: f32, b: f32) -> f32 {
    if a < 0.0 && b < 0.0 {
        return 1.0;
    }
    if a < 0.0 && b >= 0.0 {
        return a / (a - b);
    }
    if a >= 0.0 && b < 0.0 {
        return b / (b - a);
    }
    return 0.0;
}

fn solid_cycle(l: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(l.y, l.z, l.w, l.x);
}

// The fraction of the square inside the solid, corners bottom-left,
// bottom-right, top-left, top-right (LevelsetUtils::fractionInside).
fn solid_square_inside(bl: f32, br: f32, tl: f32, tr: f32) -> f32 {
    let inside = select(0, 1, bl < 0.0) + select(0, 1, tl < 0.0) + select(0, 1, br < 0.0) + select(0, 1, tr < 0.0);
    var l = vec4<f32>(bl, br, tr, tl);
    if inside == 4 {
        return 1.0;
    }
    if inside == 3 {
        for (var r = 0; r < 4 && l.x < 0.0; r = r + 1) {
            l = solid_cycle(l);
        }
        let side0 = 1.0 - solid_segment(l.x, l.w);
        let side1 = 1.0 - solid_segment(l.x, l.y);
        return 1.0 - 0.5 * side0 * side1;
    }
    if inside == 2 {
        for (var r = 0; r < 4 && (l.x >= 0.0 || !(l.y < 0.0 || l.z < 0.0)); r = r + 1) {
            l = solid_cycle(l);
        }
        if l.y < 0.0 {
            let left = solid_segment(l.x, l.w);
            let right = solid_segment(l.y, l.z);
            return 0.5 * (left + right);
        }
        let middle = 0.25 * (l.x + l.y + l.z + l.w);
        if middle < 0.0 {
            let side1 = 1.0 - solid_segment(l.x, l.w);
            let side3 = 1.0 - solid_segment(l.z, l.w);
            let side2 = 1.0 - solid_segment(l.z, l.y);
            let side0 = 1.0 - solid_segment(l.x, l.y);
            return 1.0 - (0.5 * side1 * side3 + 0.5 * side0 * side2);
        }
        let side0 = solid_segment(l.x, l.y);
        let side1 = solid_segment(l.x, l.w);
        let side2 = solid_segment(l.z, l.y);
        let side3 = solid_segment(l.z, l.w);
        return 0.5 * side0 * side1 + 0.5 * side2 * side3;
    }
    if inside == 1 {
        for (var r = 0; r < 4 && l.x >= 0.0; r = r + 1) {
            l = solid_cycle(l);
        }
        let side0 = solid_segment(l.x, l.w);
        let side1 = solid_segment(l.x, l.y);
        return 0.5 * side0 * side1;
    }
    return 0.0;
}

// The engine's corner order: U (j, k), V (k, i), W (j, i), each (0, 0),
// (1, 0), (0, 1), (1, 1) along its two cross axes.
fn cross_axes(a: i32) -> vec2<i32> {
    if a == 1 {
        return vec2<i32>(2, 0);
    }
    if a == 2 {
        return vec2<i32>(1, 0);
    }
    return vec2<i32>(1, 2);
}

// One thread per face record, the solid corners to `faces_out`: each inner
// face's open fraction clamp(1 − inside) from its four corners
// (MeshLevelSet::_getFaceWeight); a face on the interface at every corner
// takes the symmetric limit ½. Weight w of a cell's record is its open
// volume, 1 − the cube's inside fraction. Box wall faces are 0: the walls
// are closed. Velocity is 0.
@compute @workgroup_size(256)
fn open_fractions(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let tolerance = 8.0 * 1.1920929e-7 * (u.cell_size * f32(max(n.x, max(n.y, n.z))) + u.box_offset);
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) || p[a] == 0 || p[a] == n[a] {
            continue;
        }
        let axes = cross_axes(a);
        var e1 = vec3<i32>(0);
        var e2 = vec3<i32>(0);
        e1[axes.x] = 1;
        e2[axes.y] = 1;
        let c0 = solid[flatten(p, m)];
        let c1 = solid[flatten(p + e1, m)];
        let c2 = solid[flatten(p + e2, m)];
        let c3 = solid[flatten(p + e1 + e2, m)];
        var inside = solid_square_inside(c0, c1, c2, c3);
        if abs(c0) <= tolerance && abs(c1) <= tolerance && abs(c2) <= tolerance && abs(c3) <= tolerance {
            inside = 0.5;
        }
        out.face_weight[a] = clamp(1.0 - inside, 0.0, 1.0);
    }
    if all(p < n) {
        var corners: array<f32, 8>;
        for (var k = 0; k < 8; k = k + 1) {
            corners[k] = solid[flatten(p + vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1), m)];
        }
        out.face_weight.w = clamp(1.0 - solid_cube_inside(corners), 0.0, 1.0);
    }
    faces_out[idx] = out;
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

// The row of the body nearest x (smallest signed distance), −1 when no
// enabled body's lattice holds x.
fn closest_body(x: vec3<f32>) -> i32 {
    var best = -1;
    var nearest = 0.0;
    let first = max(u.rows - u.body_count, 0);
    for (var b = 0; b < u.body_count; b = b + 1) {
        let row = first + b;
        if row >= u.rows {
            break;
        }
        let bd = bodies[u32(row)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 || u32(shape_index) >= u.shapes_len {
            continue;
        }
        let position = fma(bd.linear_velocity.xyz, vec3<f32>(u.tick_seconds), bd.position_inv_mass.xyz);
        let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, u.tick_seconds);
        let sh = shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
        if !liquid_lattice_holds(g, dims) {
            continue;
        }
        let d = liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w;
        if best < 0 || d < nearest {
            best = row;
            nearest = d;
        }
    }
    return best;
}

// One thread per face record, `solid_faces` to `faces_out`. On an inner face
// a solid cuts (open fraction under 1): velocity is the normal part of the
// closest body's rigid velocity at the face centre, posed tick_seconds into
// the tick as the solid distance poses it (a dynamic body adds its predicted
// external acceleration over that time); weight is the closest body's
// friction at the face's four corners, averaged
// (FluidSimulation::_getFaceFrictionU/V/W), 0 at a corner no body's lattice
// holds. Every other face is zero.
@compute @workgroup_size(256)
fn solid_face_velocity(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    let open = solid_faces[idx];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let lattice_min = u.box_min;
    let h = u.cell_size;
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) || p[a] == 0 || p[a] == n[a] || !(open.face_weight[a] < 1.0) {
            continue;
        }
        var centre = fma(vec3<f32>(p) + vec3<f32>(0.5), vec3<f32>(h), lattice_min);
        centre[a] = fma(f32(p[a]), h, lattice_min[a]);
        let row = closest_body(centre);
        if row >= 0 {
            let bd = bodies[u32(row)];
            let position = fma(bd.linear_velocity.xyz, vec3<f32>(u.tick_seconds), bd.position_inv_mass.xyz);
            var linear = bd.linear_velocity.xyz;
            var angular = bd.angular_velocity.xyz;
            if bd.position_inv_mass.w > 0.0 {
                linear = fma(bd.accel_shape.xyz, vec3<f32>(u.tick_seconds), linear);
                angular = fma(vec3<f32>(bd.inv_inertia_x.w, bd.inv_inertia_y.w, bd.inv_inertia_z.w), vec3<f32>(u.tick_seconds), angular);
            }
            out.face_velocity[a] = liquid_body_velocity(linear, angular, position, centre)[a];
        }
        let axes = cross_axes(a);
        var friction = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            var q = p;
            q[axes.x] = q[axes.x] + (k & 1);
            q[axes.y] = q[axes.y] + ((k >> 1u) & 1);
            let at = closest_body(lattice_min + vec3<f32>(q) * h);
            if at >= 0 {
                friction = friction + bodies[u32(at)].linear_velocity.w;
            }
        }
        out.face_weight[a] = 0.25 * friction;
    }
    faces_out[idx] = out;
}

// Open fraction of face a at record f: 1 on a box wall (it holds only the
// part leaving the wall), else the solid's.
fn open_at(f: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> f32 {
    if f[a] == 0 || f[a] == n[a] {
        return 1.0;
    }
    return solid_faces[flatten(f, m)].face_weight[a];
}

// (c − w)·v_s on an inner face; 0 on a box wall, which no solid moves.
fn solid_flux(f: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>, c: f32) -> f32 {
    if f[a] == 0 || f[a] == n[a] {
        return 0.0;
    }
    let at = flatten(f, m);
    return (c - solid_faces[at].face_weight[a]) * solid_velocity[at].face_velocity[a];
}

// One thread per cell, `faces_in` to `cell_out`: in a water cell, the net
// outflow through its six faces over h, each face's velocity times its open
// fraction (PressureSolver::_calculateNegativeDivergenceVector), plus the
// solids' C·v_s term, (c − w)·v_s outward through each inner face with c the
// cell's open volume; 0 in air.
@compute @workgroup_size(256)
fn divergence(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    if !(water[idx] > 0.5) {
        cell_out[idx] = 0.0;
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    let at = flatten(p, m);
    let here = faces_in[at].face_velocity;
    let open_volume = solid_faces[at].face_weight.w;
    var outflow = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        var q = p;
        q[a] = p[a] + 1;
        let upper = open_at(q, a, n, m) * faces_in[flatten(q, m)].face_velocity[a];
        outflow = outflow + upper - open_at(p, a, n, m) * here[a]
            + solid_flux(q, a, n, m, open_volume) - solid_flux(p, a, n, m, open_volume);
    }
    cell_out[idx] = outflow / u.cell_size;
}

// One thread per cell, the particles to `cell_out`: the signed distance at
// the cell's centre, each live particle a ball of radius r = √3·h/2, the
// engine's scatter box [floor((q − 2r − min) / h), floor((q + 2r − min) / h)]
// taken as a gather. Starts at 3h; reads the 27 cells around the cell, then
// the ring two out when a particle was near and φ is still over 1.5h − r. A
// value within 0.005h of zero moves to ±0.005h by its sign, zero to −0.005h.
@compute @workgroup_size(256)
fn particle_distance(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    let n = lattice();
    let p = unflatten(idx, n);
    let h = u.cell_size;
    let centre = u.box_min + (vec3<f32>(p) + vec3<f32>(0.5)) * h;
    let radius = 0.8660254 * h;
    let search = 2.0 * radius;
    let slots = u.capacity;
    var distance = 3.0 * h;
    var near = false;
    for (var ring = 1; ring <= 2; ring = ring + 1) {
        if ring == 2 && (!near || distance <= 1.5 * h - radius) {
            break;
        }
        let first = max(p - vec3<i32>(ring), vec3<i32>(0));
        let last = min(p + vec3<i32>(ring), n - vec3<i32>(1));
        for (var z = first.z; z <= last.z; z = z + 1) {
            for (var y = first.y; y <= last.y; y = y + 1) {
                for (var x = first.x; x <= last.x; x = x + 1) {
                    let offset = abs(vec3<i32>(x, y, z) - p);
                    if ring == 2 && max(max(offset.x, offset.y), offset.z) < 2 {
                        continue;
                    }
                    let range = ranges[flatten(vec3<i32>(x, y, z), n)];
                    let start = min(range.start, slots);
                    let end = start + min(range.count, slots - start);
                    for (var s = start; s < end; s = s + 1u) {
                        let particle = sorted[s];
                        if !(particle.position_radius.w > 0.0) {
                            continue;
                        }
                        near = true;
                        let q = particle.position_radius.xyz;
                        let low = vec3<i32>(floor((q - vec3<f32>(search) - u.box_min) / h));
                        let high = vec3<i32>(floor((q + vec3<f32>(search) - u.box_min) / h));
                        if any(p < low) || any(p > high) {
                            continue;
                        }
                        distance = min(distance, length(centre - q) - radius);
                    }
                }
            }
        }
    }
    let eps = 0.005 * h;
    if abs(distance) < eps {
        distance = select(-eps, eps, distance > 0.0);
    }
    cell_out[idx] = distance;
}

// φ for the free surface: the particles' distance with `ghost` 1, zero
// (air at zero pressure on its centre) with 0.
fn surface_phi(cell: u32) -> f32 {
    return select(0.0, phi[cell], u.ghost == 1u);
}

// One thread per face record, in place on `faces_rw`. A box wall face keeps
// its velocity and is valid. A closed inner face (open fraction 0) keeps its
// velocity and is valid, for the constraint to give it the solid's
// (PressureSolver::_applyPressureToVelocityField). An open inner face beside
// water loses (p_upper − p_lower) / h, the air side's pressure the ghost
// value clamp(φ_air / (φ_water + 1e-9), −25, 25) · p_water, φ_water taken at
// most −0.005h and φ_air at least 0, exactly the rows the solve read, and is
// valid; between two air cells it keeps its velocity and is invalid (weight
// 0) for the extension to fill.
@compute @workgroup_size(256)
fn subtract_pressure(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    let here = faces_rw[idx];
    let open = solid_faces[idx];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let surface = -0.005 * u.cell_size;
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) {
            continue;
        }
        if p[a] == 0 || p[a] == n[a] {
            out.face_velocity[a] = here.face_velocity[a];
            out.face_weight[a] = 1.0;
            continue;
        }
        var below = p;
        below[a] = p[a] - 1;
        let upper = flatten(p, n);
        let lower = flatten(below, n);
        out.face_velocity[a] = here.face_velocity[a];
        let wet_upper = water[upper] > 0.5;
        let wet_lower = water[lower] > 0.5;
        if !(open.face_weight[a] > 0.0) {
            out.face_weight[a] = 1.0;
        } else if wet_upper || wet_lower {
            var p_upper = pressure[upper];
            var p_lower = pressure[lower];
            if !wet_upper {
                p_upper = clamp(max(surface_phi(upper), 0.0) / (min(surface_phi(lower), surface) + 1e-9), -25.0, 25.0) * p_lower;
            } else if !wet_lower {
                p_lower = clamp(max(surface_phi(lower), 0.0) / (min(surface_phi(upper), surface) + 1e-9), -25.0, 25.0) * p_upper;
            }
            out.face_velocity[a] = here.face_velocity[a] - (p_upper - p_lower) / u.cell_size;
            out.face_weight[a] = 1.0;
        }
    }
    faces_rw[idx] = out;
}

// One thread per face record, in place on `faces_rw`
// (FluidSimulation::_constrainVelocityFieldThread): on an inner face, a
// closed face (open fraction 0) takes the solid's velocity v_s, a cut face
// takes f·v_s + (1 − f)·u with f the solid's friction, an open face keeps u.
// Box wall faces and the weights pass through.
@compute @workgroup_size(256)
fn constrain_solid_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    var out = faces_rw[idx];
    let open = solid_faces[idx];
    let solid_here = solid_velocity[idx];
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) || p[a] == 0 || p[a] == n[a] {
            continue;
        }
        let w = open.face_weight[a];
        let v_s = solid_here.face_velocity[a];
        if !(w > 0.0) {
            out.face_velocity[a] = v_s;
        } else if w < 1.0 {
            let f = solid_here.face_weight[a];
            out.face_velocity[a] = f * v_s + (1.0 - f) * out.face_velocity[a];
        }
    }
    faces_rw[idx] = out;
}

// One thread per cell, the ranges to `cell_out`: a cell holding particles
// has crowding e = count / rest − 1. Inside the water (every neighbour in
// the lattice holds at least half of rest) its source is rate · e; at the
// surface, rate · max(e, 0), since fewer particles there only mean a
// part-full cell. Out is −source; an empty cell gives 0.
@compute @workgroup_size(256)
fn density_source(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    if ranges[idx].count == 0u {
        cell_out[idx] = 0.0;
        return;
    }
    let n = lattice();
    let p = unflatten(idx, n);
    var inside = true;
    for (var a = 0; a < 3; a = a + 1) {
        for (var side = -1; side <= 1; side = side + 2) {
            var q = p;
            q[a] = p[a] + side;
            if q[a] >= 0 && q[a] < n[a] && 2.0 * f32(ranges[flatten(q, n)].count) < u.rest {
                inside = false;
            }
        }
    }
    let crowding = f32(ranges[idx].count) / u.rest - 1.0;
    cell_out[idx] = -u.rate * select(max(crowding, 0.0), crowding, inside);
}

// A moved particle stays 0.2 cells inside each box wall, as the engine keeps
// its particles off its solids (`_solidBufferWidth`).
const WALL_MARGIN: f32 = 0.2;

// A density correction longer than half a cell carries a particle past the
// cell it was spreading from and crowds the next one.
const MAX_SPREAD: f32 = 0.5;

// The CFL guard: one RK3 stage moves at most max_travel cells. A non-finite
// v stays non-finite.
fn guard(v: vec3<f32>, per_cell: f32) -> vec3<f32> {
    let cells = length(v) * per_cell;
    return select(v, v * (u.max_travel / cells), cells > u.max_travel);
}

// Exponent bits, not x != x: fast math may fold a NaN comparison away.
fn finite(v: vec3<f32>) -> bool {
    let bits = bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u);
    return all(bits != vec3<u32>(0x7f800000u));
}

// grid: 0 the new faces, 1 `old`, 2 `advect`.
fn face_record(index: u32, grid: u32) -> FaceSample {
    if grid == 0u {
        return faces_in[index];
    }
    if grid == 1u {
        return old[index];
    }
    return advect[index];
}

// Trilinear per component over the faces with weight > 0, renormalised by
// their weights (0 when none). Every index is clamped as an integer, so a
// non-finite position reads in bounds.
fn sample(q: vec3<f32>, n: vec3<i32>, grid: u32) -> vec3<f32> {
    let m = n + vec3<i32>(1);
    var v = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var offset = vec3<f32>(0.5);
        offset[a] = 0.0;
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        let s = q - offset;
        let base = clamp(vec3<i32>(floor(s)), vec3<i32>(0), max(top - vec3<i32>(1), vec3<i32>(0)));
        let t = clamp(s - vec3<f32>(base), vec3<f32>(0.0), vec3<f32>(1.0));
        var sum = 0.0;
        var total = 0.0;
        for (var corner = 0; corner < 8; corner = corner + 1) {
            let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
            let c = min(base + bit, top);
            let face = face_record(flatten(c, m), grid);
            if face.face_weight[a] > 0.0 {
                let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
                let w = w3.x * w3.y * w3.z;
                sum = sum + w * face.face_velocity[a];
                total = total + w;
            }
        }
        v[a] = select(0.0, sum / max(total, 1e-30), total > 1e-6);
    }
    return v;
}

// One thread per particle slot, `sorted` to `particles_out`. A live particle
// (radius > 0) at q blends FLIP and PIC, flip · (v + new(q) − old(q)) +
// (1 − flip) · new(q), then moves by RK3 through the new faces (stages at ½
// and ¾ of step_dt, weights 2/9, 3/9, 4/9, each guarded), plus the spread
// step_dt · (advect(q) − new(q)) capped at MAX_SPREAD cells, kept
// WALL_MARGIN cells inside each wall. A non-finite move or velocity is
// written as it is: the tick's stats must see it to halt the liquid. Radius
// and id are kept; unused slots pass through.
@compute @workgroup_size(256)
fn faces_to_particles(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.particles {
        return;
    }
    let particle = sorted[idx];
    var out = particle;
    if !(particle.position_radius.w > 0.0) {
        particles_out[idx] = out;
        return;
    }
    let n = lattice();
    let lo = u.box_min;
    let per_cell = u.step_dt / u.cell_size;
    let q0 = (particle.position_radius.xyz - lo) / u.cell_size;
    let after = sample(q0, n, 0u);
    let k1 = guard(after, per_cell);
    let k2 = guard(sample(q0 + 0.5 * per_cell * k1, n, 0u), per_cell);
    let k3 = guard(sample(q0 + 0.75 * per_cell * k2, n, 0u), per_cell);
    let spread = per_cell * (sample(q0, n, 2u) - after);
    let spread_cells = length(spread);
    let capped = select(spread, spread * (MAX_SPREAD / spread_cells), spread_cells > MAX_SPREAD);
    let edge = vec3<f32>(WALL_MARGIN);
    let reached = q0 + per_cell * (2.0 * k1 + 3.0 * k2 + 4.0 * k3) / 9.0 + capped;
    let q1 = select(reached, clamp(reached, edge, vec3<f32>(n) - edge), finite(reached));
    let before = sample(q0, n, 1u);
    out.position_radius = vec4<f32>(lo + q1 * u.cell_size, particle.position_radius.w);
    out.velocity = u.flip * (particle.velocity + after - before) + (1.0 - u.flip) * after;
    particles_out[idx] = out;
}
