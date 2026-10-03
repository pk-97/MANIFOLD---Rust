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
// The density projection (density_source) is ported from blub (MIT,
// Copyright (c) 2020 Andreas Reich; see THIRD_PARTY_NOTICES.md):
// density_projection_gather_error.comp, its kernel, solid face weight 0.5625,
// surface clamp and source clamp.
//
// Ported from FLIP Fluids (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis
// Fassbaender; see THIRD_PARTY_NOTICES.md): velocityadvector.cpp (particles
// to faces), particlelevelset.cpp (the particle distance),
// levelsetutils.cpp and meshlevelset.cpp (the solid open fractions),
// fluidsimulation.cpp (the solids' face velocity, the constraint, and the
// particles' solid collision and removal), interpolation.cpp (the solid
// distance's gradient) and pressuresolver.cpp (divergence, the pressure
// subtraction and the sealed pockets' solid velocity). Sources and drains
// follow fluidsimulation.cpp: _updateInflowMeshFluidSource (8813-8874) and
// _addNewFluidCells (8566-8603, 8771-8811) for emission, the outflow removal
// in _updateMeshFluidSources (9011-9031), _constrainMarkerParticleVelocities
// (7397-7451) and _getInflowConstrainedVelocityComponents (6112) for the
// constrained velocity.

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
    // The largest |box_min| component, for the solid faces' tolerance.
    box_offset: f32,
    // subtract: 1 reads `phi` for the free surface, 0 keeps air at zero.
    ghost: u32,
    // Particles the move writes.
    particles: u32,
    // Records `shapes` holds.
    shapes_len: u32,
    // The density projection's source scale, 1 / step_dt.
    rate: f32,
    // The tank's closed faces: bit 2d the low face of axis d, bit 2d + 1 the
    // high one.
    closed_faces: u32,
    // Inflow and outflow regions a tick, and the region rows `regions` holds
    // (tick major from first_tick).
    region_count: i32,
    region_rows: i32,
    // Half-width of an emitted particle's jitter in cells, a quarter of the
    // jitter factor (_getMarkerParticleJitter).
    emit_jitter: f32,
    // The V-cycle level the pressure solves run on; the pocket coarsening
    // reads it.
    solve_level: u32,
    // 1: every tile is active (the test-only oracle).
    all_tiles: u32,
    // The ring a sparse pass's reads are capped at.
    ring_cap: u32,
    // The farthest ring the table holds; ring_max + 1 means none within it.
    ring_max: u32,
    tile_pad: u32,
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
@group(0) @binding(18) var<storage, read> spread: array<FaceSample>;
@group(0) @binding(19) var<storage, read_write> particles_out: array<FluidParticle>;
@group(0) @binding(20) var<storage, read_write> faces_rw: array<FaceSample>;
// 8 floats per body: the linear and angular impulse the water has put on it
// so far this tick (gpu_flip_bodies.wgsl).
@group(0) @binding(21) var<storage, read> reaction: array<f32>;
// Two words per particle slot, summed over the tick's substeps: RK3 stages
// the CFL guard shortened, and solid push-outs refused past SOLID_PUSH. The
// tick's stats reduce them (liquid_stats words 8 and 9).
@group(0) @binding(22) var<storage, read_write> capped: array<u32>;
// Inflow (code 2) and outflow (code 3) rows: LiquidBody rows with the code in
// angular_velocity.w, the emitted velocity in inv_inertia_x.xyz and the share
// of the region's own motion added to it in inv_inertia_x.w.
@group(0) @binding(36) var<storage, read> regions: array<LiquidBody>;
// One word per half-cell site: the emission flags, scanned in place.
@group(0) @binding(37) var<storage, read_write> emit_scan: array<u32>;
// The sorted particles, written past the live ones by emit_write.
@group(0) @binding(38) var<storage, read_write> emitted: array<FluidParticle>;

// Set by resolve_solid when it refuses a push-out past SOLID_PUSH.
var<private> push_refused: u32 = 0u;

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

// One thread per face record. Each face sums the engine's Wyvill weight
// 1 − (4/9)·s³/r⁶ + (17/9)·s²/r⁴ − (22/9)·s/r² for s = |q − face|² < r²,
// r = √3/2 cells, over every live particle in the 3 × 3 × 3 cells around p,
// and the weighted velocity along its normal. A face over weight 1e-6 gets
// the ratio; any other gets velocity 0 and weight 0 for the extension to
// fill. A box wall face is closed: velocity 0, valid (weight 1), so the
// extension never writes it (the engine's domain boundary, weight 0 in the
// solve, its velocity the static solid's).
@compute @workgroup_size(256)
fn particles_to_faces(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = c_face_index(gid.x);
    if idx == NO_CELL {
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
            velocity[a] = 0.0;
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

// The world-space signed distance at x to region r of this substep's tick,
// posed tick_seconds into the tick as closest_body poses a body; far
// outside (1e30) when the region is off or its lattice does not hold x.
fn region_distance(r: i32, x: vec3<f32>) -> f32 {
    let row = (u.tick_index - u.first_tick) * u.region_count + r;
    if row < 0 || row >= u.region_rows {
        return 1e30;
    }
    let bd = regions[u32(row)];
    let shape_index = i32(bd.accel_shape.w);
    if shape_index < 0 || u32(shape_index) >= u.shapes_len {
        return 1e30;
    }
    let position = fma(bd.linear_velocity.xyz, vec3<f32>(u.tick_seconds), bd.position_inv_mass.xyz);
    let q = liquid_turn(bd.rotation, bd.angular_velocity.xyz, u.tick_seconds);
    let sh = shapes[u32(shape_index)];
    let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
    let g = liquid_lattice_coord(x, position, q, sh.origin_spacing, sh.scale_min.xyz);
    if !liquid_lattice_holds(g, dims) {
        return 1e30;
    }
    return liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w;
}

// The first region of kind `code` (2 inflow, 3 outflow) holding x: distance
// at or below 0 when `closed`, below 0 otherwise; −1 when none does.
fn region_holding(x: vec3<f32>, code: f32, closed: bool) -> i32 {
    let first = (u.tick_index - u.first_tick) * u.region_count;
    for (var r = 0; r < u.region_count; r = r + 1) {
        let row = first + r;
        if row < 0 || row >= u.region_rows || regions[u32(row)].angular_velocity.w != code {
            continue;
        }
        let d = region_distance(r, x);
        if d < 0.0 || (closed && d <= 0.0) {
            return r;
        }
    }
    return -1;
}

// An inflow's velocity at x: its authored velocity plus its share of the
// region's own rigid motion there (the engine's append-object-velocity).
fn region_velocity(r: i32, x: vec3<f32>) -> vec3<f32> {
    let bd = regions[u32((u.tick_index - u.first_tick) * u.region_count + r)];
    let position = fma(bd.linear_velocity.xyz, vec3<f32>(u.tick_seconds), bd.position_inv_mass.xyz);
    let rigid = bd.linear_velocity.xyz + cross(bd.angular_velocity.xyz, x - position);
    return bd.inv_inertia_x.xyz + bd.inv_inertia_x.w * rigid;
}

// One thread per face record, `faces_in` to `faces_out`: each face gains
// step_dt · (g + forces(x)) along its normal a, x its centre, plus the
// impulses on step 0 of impulse_tick, read from the domain's coarse field
// lattices (origin the box minimum). A box wall face stays 0. Weights pass
// through.
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
        // A valid face inside an inflow takes no body force
        // (_getInflowConstrainedVelocityComponents 6112-6175, before the body
        // forces; valid is weight > 0, the engine's _validVelocities after
        // extension). Nothing pins faces after the solve: the engine's
        // _constrainVelocityFields is solids only.
        if u.region_count > 0 && here.face_weight[a] > 0.0 && region_holding(x, 2.0, false) >= 0 {
            accel = 0.0;
        }
        var v = fma(accel, u.step_dt, here.face_velocity[a]);
        if impulse {
            v = v + gravity_impulse(x, origin, u32(a));
        }
        out.face_velocity[a] = select(v, 0.0, p[a] == 0 || p[a] == n[a]);
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
// the tick as the solid distance poses it; weight is the closest body's
// friction at the face's four corners, averaged
// (FluidSimulation::_getFaceFrictionU/V/W), 0 at a corner no body's lattice
// holds. Every other face is zero. A dynamic body (1/m > 0) moves at its
// predicted velocity, as RigidFluidCoupling::beginSubstep predicts it: its
// external acceleration over tick_seconds plus M⁻¹ times the reaction so
// far this tick. Velocity w is the record's owner code
// (gpu_flip_bodies.wgsl): each face's body, counted from this tick's first
// row. Weight w is the known mask: bit a set where axis a was sampled.
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
    let first = max(u.rows - u.body_count, 0);
    var code = 0.0;
    var known = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) || p[a] == 0 || p[a] == n[a] {
            continue;
        }
        // Only a face a solid covers is sampled (weight > 0 in
        // MeshLevelSet::_computeVelocityGridThread); solid_extrapolate
        // carries the samples out over the open faces.
        if !(open.face_weight[a] < 1.0) {
            continue;
        }
        known = known + f32(1u << u32(a));
        var centre = fma(vec3<f32>(p) + vec3<f32>(0.5), vec3<f32>(h), lattice_min);
        centre[a] = fma(f32(p[a]), h, lattice_min[a]);
        let row = closest_body(centre);
        if row >= 0 {
            let bd = bodies[u32(row)];
            let body = row - first;
            let position = fma(bd.linear_velocity.xyz, vec3<f32>(u.tick_seconds), bd.position_inv_mass.xyz);
            var linear = bd.linear_velocity.xyz;
            var angular = bd.angular_velocity.xyz;
            if bd.position_inv_mass.w > 0.0 {
                let r = 8u * u32(body);
                let push = vec3<f32>(reaction[r], reaction[r + 1u], reaction[r + 2u]);
                let turn = vec3<f32>(reaction[r + 4u], reaction[r + 5u], reaction[r + 6u]);
                linear = fma(bd.accel_shape.xyz, vec3<f32>(u.tick_seconds), linear) + bd.position_inv_mass.w * push;
                angular = fma(vec3<f32>(bd.inv_inertia_x.w, bd.inv_inertia_y.w, bd.inv_inertia_z.w), vec3<f32>(u.tick_seconds), angular)
                    + vec3<f32>(dot(bd.inv_inertia_x.xyz, turn), dot(bd.inv_inertia_y.xyz, turn), dot(bd.inv_inertia_z.xyz, turn));
            }
            out.face_velocity[a] = liquid_body_velocity(linear, angular, position, centre)[a];
            code = code + f32(body + 1) * f32(1u << (8u * u32(a)));
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
    out.face_velocity.w = code;
    out.face_weight.w = known;
    faces_out[idx] = out;
}

// Layers of the solid velocity's extrapolation
// (MeshLevelSet::_numVelocityExtrapolationLayers).
const SOLID_LAYERS: u32 = 5u;

// A face on the border of axis a's face lattice: on a box wall, or in the
// first or last layer across it. The engine holds these done from the
// start (GridUtils::_initializeStatusGridThread): never extrapolated, never
// a seed, but counted with their value in a neighbour's mean.
fn solid_border(p: vec3<i32>, a: i32, n: vec3<i32>) -> bool {
    var top = n - vec3<i32>(1);
    top[a] = n[a];
    return any(p == vec3<i32>(0)) || any(p == top);
}

fn solid_known(s: FaceSample, a: i32) -> bool {
    return ((u32(s.face_weight.w) >> u32(a)) & 1u) != 0u;
}

// One thread per face record, `faces_in` to `faces_out`: one layer of the
// solid velocity's extrapolation (MACVelocityField::extrapolateVelocityField
// for every solid, GridUtils::extrapolateGridWithObserver). An unknown inner
// face with a known neighbour in its axis's lattice takes the mean of its
// known and border neighbours and is known from the next layer; the owner
// code goes with the first owned known neighbour, so a dynamic body's
// reaction reaches the faces its velocity reaches (the engine's
// RigidBoundaryVelocityMap::extrapolate). Run SOLID_LAYERS times.
@compute @workgroup_size(256)
fn solid_extrapolate(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    var out = faces_in[idx];
    var known = u32(out.face_weight.w);
    var code = u32(out.face_velocity.w);
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) || solid_border(p, a, n) || solid_known(out, a) {
            continue;
        }
        var top = n - vec3<i32>(1);
        top[a] = n[a];
        var sum = 0.0;
        var count = 0.0;
        var seeded = false;
        var owner = 0u;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                if q[b] < 0 || q[b] > top[b] {
                    continue;
                }
                let s = faces_in[flatten(q, m)];
                let border = solid_border(q, a, n);
                if border || solid_known(s, a) {
                    sum = sum + s.face_velocity[a];
                    count = count + 1.0;
                }
                if !border && solid_known(s, a) {
                    seeded = true;
                    let o = (u32(s.face_velocity.w) >> (8u * u32(a))) & 255u;
                    if owner == 0u {
                        owner = o;
                    }
                }
            }
        }
        if seeded {
            out.face_velocity[a] = sum / count;
            known = known | (1u << u32(a));
            code = code | (owner << (8u * u32(a)));
        }
    }
    out.face_velocity.w = f32(code);
    out.face_weight.w = f32(known);
    faces_out[idx] = out;
}

// The solid distance at cell p's centre, the mean of its eight corners
// (MeshLevelSet::getDistanceAtCellCenter).
fn solid_centre(p: vec3<i32>, m: vec3<i32>) -> f32 {
    var sum = 0.0;
    for (var k = 0; k < 8; k = k + 1) {
        sum = sum + solid[flatten(p + vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1), m)];
    }
    return 0.125 * sum;
}

// A cell whose centre is inside a solid and within h/2 of the particles
// (φ under h/2) is water in the solves, its φ −h/2
// (ParticleLevelSet::postProcessSignedDistanceField). Without it a cut face
// between water and the solid's empty inside is a free surface, and the
// water beside a solid drains into it at every step.
//
// One thread per cell, in place on `cell_out`: the particles' φ.
@compute @workgroup_size(256)
fn phi_into_solids(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = c_cell_index(gid.x);
    if idx == NO_CELL {
        return;
    }
    let n = lattice();
    if cell_out[idx] < 0.5 * u.cell_size && solid_centre(unflatten(idx, n), n + vec3<i32>(1)) < 0.0 {
        cell_out[idx] = -0.5 * u.cell_size;
    }
}

// One thread per cell, `phi` to `cell_out`: water is φ < 0, after
// `phi_into_solids`. The engine's pressure cells are its liquid SDF's
// negative cells (PressureSolver::_initialize), which reach 0.866h past the
// particles rather than stopping at the cells that hold one.
@compute @workgroup_size(256)
fn water_from_phi(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = c_cell_index(gid.x);
    if idx == NO_CELL {
        return;
    }
    cell_out[idx] = select(0.0, 1.0, phi[idx] < 0.0);
}

// Sealed pockets (PressureSolver::_conditionSolidVelocityField): water a
// solid closes off from air cannot take the solid's push, so its solid face
// velocity is zeroed and the pressure solve stays consistent. Water cells
// link through a face whose open fraction is at least POCKET_LINK. A water
// cell touches air through a linked face to a dry cell
// (_computeBordersAirGridThread) or on an open tank face, which drains
// (open_band). Whether a region reaches air spreads from those cells by line
// sweeps along each axis, repeated until a round changes nothing.
@group(0) @binding(23) var<storage, read_write> pocket: array<u32>;
// Words 0-8: the three sweeps' indirect dispatch sizes (x, y, z); 9: a
// sweep changed a cell this round; 10: a sealed cell still links to one
// that reaches air (the spread stopped at its cap unfinished).
@group(0) @binding(24) var<storage, read_write> pocket_gate: array<u32>;
// Each water cell's pocket: the lowest cell index of the sealed water it
// links to. A pocket's label is its leader cell.
@group(0) @binding(25) var<storage, read_write> pocket_label: array<u32>;
// Per label, three words: a right-hand side sum as a 64-bit fixed-point
// integer (low, high) and the cell count. Then two words: the magnitude of
// everything removed this solve, in the same fixed point.
@group(0) @binding(26) var<storage, read_write> pocket_sum: array<atomic<u32>>;
// The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
// table)): 8³ tiles, T = ceil(n / 8) per axis, x fastest, partial edge tiles.
// Per tile, the Chebyshev cell distance from its box to the nearest
// particle-holding cell, 0..=CELL_REACH, or CELL_REACH + 1 beyond.
@group(0) @binding(27) var<storage, read_write> tile_near: array<u32>;
// Two halves of one tile each: the tile's list rank (rank_of), 0 in the
// cell set C, 1 for the rest of rings 0 and 1, else its ring (ring_max + 1
// beyond). The parity word of `tile_counts` names the current half.
@group(0) @binding(28) var<storage, read_write> tile_rank: array<u32>;
// Every tile: the cell set C (ring <= 1 and near <= CELL_REACH) first, then
// the rest of rings 0 and 1, then ring by ring (tiles_lists).
@group(0) @binding(29) var<storage, read_write> tiles_by_ring: array<u32>;
// Word 0 |C|, word k = 1..=ring_max + 1 the tiles with ring <= k (the last
// is every tile), the retired count at ring_max + 2, the parity at
// ring_max + 3.
@group(0) @binding(30) var<storage, read_write> tile_counts: array<u32>;
// Indirect triples [2 · count, 1, 1] (512 threads a tile, 256 a group): one
// per count word k = 0..=ring_max, then the retired list.
@group(0) @binding(31) var<storage, read_write> tile_args: array<u32>;
// Tiles in C on the previous step and not on this one.
@group(0) @binding(32) var<storage, read_write> tiles_retired: array<u32>;
// The cell arrays the C passes own, for the canonical fill and the retire
// (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 4 (The defined-value rule)); the
// gathered faces bind at `faces_out`.
@group(0) @binding(33) var<storage, read_write> tile_water: array<f32>;
@group(0) @binding(34) var<storage, read_write> tile_phi: array<f32>;
@group(0) @binding(35) var<storage, read_write> tile_rhs: array<f32>;

// Fixed point for the pocket sums: integer adds give the same total in any
// order, so the solves stay the same on every run.
const POCKET_SCALE: f32 = 65536.0;
const TWO_32: f32 = 4294967296.0;

const POCKET_DRY: u32 = 0u;
const POCKET_SEALED: u32 = 1u;
const POCKET_AIR: u32 = 2u;
const POCKET_LINK: f32 = 1e-6;
const POCKET_CHANGED: u32 = 9u;
const POCKET_UNRESOLVED: u32 = 10u;
// The pocket count's word among the solver words `capped` is bound at.
const POCKET_WORD: u32 = 3u;

fn pocket_linked(c: vec3<i32>, d: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> bool {
    // c and d are neighbours along a; the face between is the higher's low face.
    var f = c;
    f[a] = max(c[a], d[a]);
    return open_at(f, a, n, m) >= POCKET_LINK;
}

// One thread per cell: dry, sealed, or water touching air.
@compute @workgroup_size(256)
fn pocket_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    if !(water[idx] > 0.5) {
        pocket[idx] = POCKET_DRY;
        return;
    }
    pocket_label[idx] = idx;
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    var air = false;
    for (var a = 0; a < 3; a = a + 1) {
        let low_open = (u.closed_faces & (1u << u32(2 * a))) == 0u;
        let high_open = (u.closed_faces & (1u << u32(2 * a + 1))) == 0u;
        if (low_open && p[a] == 0) || (high_open && p[a] == n[a] - 1) {
            air = true;
        }
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            if d[a] >= 0 && d[a] < n[a] && !(water[flatten(d, n)] > 0.5) && pocket_linked(p, d, a, n, m) {
                air = true;
            }
        }
    }
    pocket[idx] = select(POCKET_SEALED, POCKET_AIR, air);
}

// One thread: the first round runs.
@compute @workgroup_size(1)
fn pocket_start() {
    pocket_gate[POCKET_CHANGED] = 1u;
    pocket_gate[POCKET_UNRESOLVED] = 0u;
}

// One thread: this round's sweeps run only when the last round changed a
// cell.
@compute @workgroup_size(1)
fn pocket_round() {
    let go = pocket_gate[POCKET_CHANGED] != 0u;
    let n = u.n;
    let lines = vec3<u32>(n.y * n.z, n.z * n.x, n.x * n.y);
    for (var a = 0u; a < 3u; a = a + 1u) {
        pocket_gate[3u * a] = select(0u, (lines[a] + 255u) / 256u, go);
        pocket_gate[3u * a + 1u] = 1u;
        pocket_gate[3u * a + 2u] = 1u;
    }
    pocket_gate[POCKET_CHANGED] = 0u;
}

// Cell c from its line neighbour b: sealed water linked to water reaching
// air reaches air too; linked sealed water takes the lower label.
fn pocket_step(c: vec3<i32>, b: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> bool {
    let at = flatten(c, n);
    let near = flatten(b, n);
    if pocket[at] != POCKET_SEALED || pocket[near] == POCKET_DRY || !pocket_linked(c, b, a, n, m) {
        return false;
    }
    if pocket[near] == POCKET_AIR {
        pocket[at] = POCKET_AIR;
        return true;
    }
    if pocket_label[near] < pocket_label[at] {
        pocket_label[at] = pocket_label[near];
        return true;
    }
    return false;
}

// One thread per line along axis a, forward then back; each line is its
// thread's alone.
fn pocket_sweep(t: u32, a: i32) {
    let n = lattice();
    let m = n + vec3<i32>(1);
    let axes = cross_axes(a);
    if t >= u32(n[axes.x] * n[axes.y]) {
        return;
    }
    var p = vec3<i32>(0);
    p[axes.x] = i32(t % u32(n[axes.x]));
    p[axes.y] = i32(t / u32(n[axes.x]));
    var changed = false;
    for (var i = 1; i < n[a]; i = i + 1) {
        var c = p;
        c[a] = i;
        var b = p;
        b[a] = i - 1;
        changed = pocket_step(c, b, a, n, m) || changed;
    }
    for (var i = n[a] - 2; i >= 0; i = i - 1) {
        var c = p;
        c[a] = i;
        var b = p;
        b[a] = i + 1;
        changed = pocket_step(c, b, a, n, m) || changed;
    }
    if changed {
        pocket_gate[POCKET_CHANGED] = 1u;
    }
}

@compute @workgroup_size(256)
fn pocket_sweep_x(@builtin(global_invocation_id) gid: vec3<u32>) {
    pocket_sweep(gid.x, 0);
}

@compute @workgroup_size(256)
fn pocket_sweep_y(@builtin(global_invocation_id) gid: vec3<u32>) {
    pocket_sweep(gid.x, 1);
}

@compute @workgroup_size(256)
fn pocket_sweep_z(@builtin(global_invocation_id) gid: vec3<u32>) {
    pocket_sweep(gid.x, 2);
}

// A sealed cell with a linked neighbour of the given state.
fn pocket_neighbour(c: vec3<i32>, state: u32, n: vec3<i32>, m: vec3<i32>) -> bool {
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = c;
            d[a] = c[a] + s;
            if d[a] >= 0 && d[a] < n[a] && pocket[flatten(d, n)] == state && pocket_linked(c, d, a, n, m) {
                return true;
            }
        }
    }
    return false;
}

// One thread per cell, after the last round: a sealed cell still linked to
// one that reaches air means the spread hit its cap unfinished.
@compute @workgroup_size(256)
fn pocket_check(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    let n = lattice();
    let c = unflatten(idx, n);
    if pocket[idx] != POCKET_SEALED {
        return;
    }
    let m = n + vec3<i32>(1);
    var unfinished = pocket_neighbour(c, POCKET_AIR, n, m);
    // A pocket still split between two labels: each part's mean is removed
    // apart, which keeps the solve consistent but spreads unevenly.
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = c;
            d[a] = c[a] + s;
            if d[a] >= 0 && d[a] < n[a] && pocket[flatten(d, n)] == POCKET_SEALED && pocket_label[flatten(d, n)] != pocket_label[idx] && pocket_linked(c, d, a, n, m) {
                unfinished = true;
            }
        }
    }
    if unfinished {
        pocket_gate[POCKET_UNRESOLVED] = 1u;
    }
}

// One thread per cell, `water` to `cell_out`: the solves' water, less each
// sealed pocket's leader cell. With no air a pocket's L is singular, and in
// f32 its residual never reaches the stop; the leader then stands as air at
// p = 0, an identity row, and its neighbours see it as a pressure-0
// neighbour as the subtract pass does.
@compute @workgroup_size(256)
fn pocket_pin(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    let leader = pocket[idx] == POCKET_SEALED && pocket_label[idx] == idx;
    cell_out[idx] = select(water[idx], 0.0, leader);
}

// Separating solids (GPU_FLIP_PRESSURE_SOLVE.md section 8 (Separating
// solids)): 1 where a water cell touching a solid is let go, its pressure held
// at 0 and its leftover divergence free to be outflow. Carried step to step.
@group(0) @binding(42) var<storage, read_write> let_go: array<f32>;

// A water cell with any face less than fully open: a box wall or a body.
fn touches_solid(p: vec3<i32>, n: vec3<i32>, m: vec3<i32>) -> bool {
    for (var a = 0; a < 3; a = a + 1) {
        var q = p;
        q[a] = p[a] + 1;
        if open_at(p, a, n, m) < 1.0 || open_at(q, a, n, m) < 1.0 {
            return true;
        }
    }
    return false;
}

// One thread per cell, `water` the solve mask to `cell_out` the contact mask:
// the let-go set kept only on water touching a solid (and emptied on the
// first step of the first tick), each let-go cell taken out of the mask.
@compute @workgroup_size(256)
fn separate_pin(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let first = u.tick_index == 0 && u.step_in_tick == 0;
    let keep = !first && let_go[idx] > 0.5 && water[idx] > 0.5 && touches_solid(unflatten(idx, n), n, m);
    let_go[idx] = select(0.0, 1.0, keep);
    cell_out[idx] = select(water[idx], 0.0, keep);
}

// One thread per cell, after the projection and the bodies' reaction,
// `water` the contact mask and `cell_out` (read only) the divergence of the
// projected faces against the solids' updated face velocity: one active-set
// update. A pressing cell whose pressure came out negative is let go; a
// let-go cell whose leftover divergence is negative (water pushed into the
// solid, relative to the solid's own motion) presses again.
@compute @workgroup_size(256)
fn separate_update(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    if water[idx] > 0.5 {
        if pressure[idx] < 0.0 && touches_solid(p, n, m) {
            let_go[idx] = 1.0;
        }
        return;
    }
    if !(let_go[idx] > 0.5) {
        return;
    }
    if cell_out[idx] < 0.0 {
        let_go[idx] = 0.0;
    }
}

// One thread per word: the pocket sums start at 0.
@compute @workgroup_size(256)
fn pocket_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x < 3u * cell_total() + 2u {
        atomicStore(&pocket_sum[gid.x], 0u);
    }
}

// A cell's right-hand side in the sums' fixed point, as (low, high) of a
// 64-bit two's-complement integer. The value the solve then sees is this
// rounded one, so a pocket sums to exactly its removed mean.
fn pocket_fixed(x: f32) -> vec2<u32> {
    let v = round(x * POCKET_SCALE);
    // Split the magnitude, which is exact for an integral f32, then negate.
    let m = abs(v);
    let mh = floor(m / TWO_32);
    let q = vec2<u32>(u32(m - mh * TWO_32), u32(mh));
    return select(q, pocket_negate(q), v < 0.0);
}

fn pocket_negate(q: vec2<u32>) -> vec2<u32> {
    let low = ~q.x + 1u;
    return vec2<u32>(low, ~q.y + select(0u, 1u, low == 0u));
}

// Decoded through the magnitude, so a small negative keeps its precision.
fn pocket_value(low: u32, high: u32) -> f32 {
    let negative = (high & 0x80000000u) != 0u;
    let m = select(vec2<u32>(low, high), pocket_negate(vec2<u32>(low, high)), negative);
    let magnitude = (f32(m.y) * TWO_32 + f32(m.x)) / POCKET_SCALE;
    return select(magnitude, -magnitude, negative);
}

// Adds (low, high) at word w with the carry; the total is exact in any order.
fn pocket_add(w: u32, q: vec2<u32>) {
    let old = atomicAdd(&pocket_sum[w], q.x);
    let carry = select(0u, 1u, old + q.x < old);
    atomicAdd(&pocket_sum[w + 1u], q.y + carry);
}

var<workgroup> group_label: u32;
var<workgroup> group_sum: array<atomic<u32>, 3>;

// One thread per cell, the right-hand side in `cell_out`: each sealed cell
// adds itself to its pocket. A workgroup first folds the cells sharing its
// first sealed cell's label, so a large pocket is not one contended word.
@compute @workgroup_size(256)
fn pocket_accumulate(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let idx = gid.x;
    if lane == 0u {
        group_label = 0xffffffffu;
        atomicStore(&group_sum[0], 0u);
        atomicStore(&group_sum[1], 0u);
        atomicStore(&group_sum[2], 0u);
    }
    workgroupBarrier();
    let sealed = idx < cell_total() && pocket[idx] == POCKET_SEALED;
    var label = 0u;
    if sealed {
        label = pocket_label[idx];
        // Any one sealed lane's label wins; the rest go straight to memory.
        group_label = label;
    }
    workgroupBarrier();
    let shared_label = workgroupUniformLoad(&group_label);
    if sealed {
        let q = pocket_fixed(cell_out[idx]);
        if label == shared_label {
            let old = atomicAdd(&group_sum[0], q.x);
            let carry = select(0u, 1u, old + q.x < old);
            atomicAdd(&group_sum[1], q.y + carry);
            atomicAdd(&group_sum[2], 1u);
        } else {
            pocket_add(3u * label, q);
            atomicAdd(&pocket_sum[3u * label + 2u], 1u);
        }
    }
    workgroupBarrier();
    if lane == 0u && shared_label != 0xffffffffu {
        pocket_add(3u * shared_label, vec2<u32>(atomicLoad(&group_sum[0]), atomicLoad(&group_sum[1])));
        atomicAdd(&pocket_sum[3u * shared_label + 2u], atomicLoad(&group_sum[2]));
    }
}

// One thread per cell: a sealed cell's right-hand side less its pocket's
// mean, so each pocket sums to 0 and its pure-Neumann solve has a solution.
// The leader cell adds what its pocket lost to the removed total.
@compute @workgroup_size(256)
fn pocket_remove(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() || pocket[idx] != POCKET_SEALED {
        return;
    }
    let label = pocket_label[idx];
    let low = atomicLoad(&pocket_sum[3u * label]);
    let high = atomicLoad(&pocket_sum[3u * label + 1u]);
    let count = atomicLoad(&pocket_sum[3u * label + 2u]);
    let q = pocket_fixed(cell_out[idx]);
    cell_out[idx] = pocket_value(q.x, q.y) - pocket_value(low, high) / f32(count);
    if idx == label {
        let total = pocket_value(low, high);
        pocket_add(3u * cell_total(), pocket_fixed(abs(total)));
    }
}

// One thread, `capped` bound at the solver words: the volume rate removed
// from this step's pockets, h³ times the removed sum, added to `word`.
fn pocket_flux(word: u32) {
    let at = 3u * cell_total();
    let total = pocket_value(atomicLoad(&pocket_sum[at]), atomicLoad(&pocket_sum[at + 1u]));
    let h = u.cell_size;
    let removed = total * h * h * h;
    if u.step_in_tick == 0 {
        capped[word] = bitcast<u32>(removed);
    } else {
        capped[word] = bitcast<u32>(bitcast<f32>(capped[word]) + removed);
    }
}

@compute @workgroup_size(1)
fn pocket_flux_pressure() {
    // The density word too: a tick without the density projection reads 0.
    if u.step_in_tick == 0 {
        capped[5u] = 0u;
    }
    pocket_flux(4u);
}

@compute @workgroup_size(1)
fn pocket_flux_density() {
    pocket_flux(5u);
}

// Solve Level (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11): the pockets at
// the solve's level. A level cell is sealed when every fine cell under it
// is sealed with one label; its label is the lowest level cell of that
// pocket, so the pocket passes run over the level's right-hand side
// unchanged.
@group(0) @binding(39) var<storage, read_write> pocket_coarse: array<u32>;
@group(0) @binding(40) var<storage, read_write> pocket_coarse_label: array<u32>;
// By fine label: the lowest level cell of that pocket.
@group(0) @binding(41) var<storage, read_write> pocket_leader: array<atomic<u32>>;

const NO_LEADER: u32 = 0xffffffffu;

// The lattice `k` halvings down, each rounding up: the solver's levels.
fn level_lattice(k: u32) -> vec3<i32> {
    var n = lattice();
    for (var j = 0u; j < k; j = j + 1u) {
        n = (n + 1) / 2;
    }
    return n;
}

fn level_total(k: u32) -> u32 {
    let c = level_lattice(k);
    return u32(c.x * c.y * c.z);
}

// One thread per fine cell: no pocket has a leader yet.
@compute @workgroup_size(256)
fn pocket_leader_clear(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x < cell_total() {
        atomicStore(&pocket_leader[gid.x], NO_LEADER);
    }
}

// A fine cell the solver coarsens as solid (gpu_flip_pressure.wgsl kind):
// not water, every face closed. It is no part of any pocket.
fn cell_solid(p: vec3<i32>, n: vec3<i32>, m: vec3<i32>) -> bool {
    if water[flatten(p, n)] > 0.5 {
        return false;
    }
    for (var a = 0; a < 3; a = a + 1) {
        var above = p;
        above[a] = p[a] + 1;
        if open_at(p, a, n, m) != 0.0 || open_at(above, a, n, m) != 0.0 {
            return false;
        }
    }
    return true;
}

// One thread per level cell: sealed with its children's fine label when
// every in-lattice child that is not solid is sealed under that one label,
// dry otherwise. The solver's level cell is water with solid children
// inside it; they carry no label and never break the seal.
@compute @workgroup_size(256)
fn pocket_coarsen(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let k = u.solve_level;
    if idx >= level_total(k) {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let c = level_lattice(k);
    let side = 1 << k;
    let base = unflatten(idx, c) * side;
    var sealed = true;
    var label = NO_LEADER;
    for (var z = 0; z < side && sealed; z = z + 1) {
        for (var y = 0; y < side && sealed; y = y + 1) {
            for (var x = 0; x < side && sealed; x = x + 1) {
                let p = base + vec3<i32>(x, y, z);
                if any(p >= n) || cell_solid(p, n, m) {
                    continue;
                }
                let f = flatten(p, n);
                if pocket[f] != POCKET_SEALED {
                    sealed = false;
                } else if label == NO_LEADER {
                    label = pocket_label[f];
                } else if pocket_label[f] != label {
                    sealed = false;
                }
            }
        }
    }
    // A level cell of solid children only is dry: it has no label to seal under.
    sealed = sealed && label != NO_LEADER;
    pocket_coarse[idx] = select(POCKET_DRY, POCKET_SEALED, sealed);
    pocket_coarse_label[idx] = label;
    if sealed {
        atomicMin(&pocket_leader[label], idx);
    }
}

// One thread per level cell: a sealed cell takes its pocket's leader.
@compute @workgroup_size(256)
fn pocket_relabel(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= level_total(u.solve_level) || pocket_coarse[idx] != POCKET_SEALED {
        return;
    }
    pocket_coarse_label[idx] = atomicLoad(&pocket_leader[pocket_coarse_label[idx]]);
}

// A cell of a sealed region of more than one cell; a lone sealed cell keeps
// its solid velocity, as the engine skips a group of one.
fn pocket_isolated(c: vec3<i32>, n: vec3<i32>, m: vec3<i32>) -> bool {
    return pocket[flatten(c, n)] == POCKET_SEALED && pocket_neighbour(c, POCKET_SEALED, n, m);
}

// One thread per face record, in place on the solid velocity in
// `faces_out`: each of the six faces of an isolated cell takes velocity 0.
@compute @workgroup_size(256)
fn pocket_condition(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, m);
    for (var a = 0; a < 3; a = a + 1) {
        if !face_exists(p, n, a) {
            continue;
        }
        var below = p;
        below[a] = p[a] - 1;
        if (p[a] < n[a] && pocket_isolated(p, n, m)) || (p[a] > 0 && pocket_isolated(below, n, m)) {
            faces_out[idx].face_velocity[a] = 0.0;
        }
    }
}

// The solver words holding the step's dry, sealed and air cell counts.
const POCKET_COUNT_WORD: u32 = 7u;
var<workgroup> pocket_counts: array<atomic<u32>, 3>;
var<workgroup> pocket_first_seed: atomic<u32>;
var<workgroup> pocket_dry_floor: atomic<u32>;
const POCKET_DRY_FLOOR_WORD: u32 = 16u;

// A floor cell (y = 0) reading dry with water in every in-box face neighbour:
// a hole under the water, not its edge.
fn dry_floor_hole(idx: u32) -> bool {
    let n = lattice();
    let p = unflatten(idx, n);
    if p.y != 0 || water[idx] > 0.5 {
        return false;
    }
    for (var a = 0; a < 3; a = a + 1) {
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            if d[a] >= 0 && d[a] < n[a] && !(water[flatten(d, n)] > 0.5) {
                return false;
            }
        }
    }
    return true;
}
const POCKET_SEED_WORD: u32 = 10u;

// Why water cell idx touches air, as pocket_seed decides it: the neighbour's
// index (0xffffffff for an open box face), the face's open fraction bits,
// and axis * 2 + (1 on the high side); x is 0xffffffff when it does not.
fn pocket_seed_reason(idx: u32) -> vec4<u32> {
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    for (var a = 0; a < 3; a = a + 1) {
        let low_open = (u.closed_faces & (1u << u32(2 * a))) == 0u;
        let high_open = (u.closed_faces & (1u << u32(2 * a + 1))) == 0u;
        if low_open && p[a] == 0 {
            return vec4<u32>(0xffffffffu, 0u, u32(2 * a), 0u);
        }
        if high_open && p[a] == n[a] - 1 {
            return vec4<u32>(0xffffffffu, 0u, u32(2 * a + 1), 0u);
        }
        for (var s = -1; s <= 1; s = s + 2) {
            var d = p;
            d[a] = p[a] + s;
            if d[a] >= 0 && d[a] < n[a] && !(water[flatten(d, n)] > 0.5) && pocket_linked(p, d, a, n, m) {
                var f = p;
                f[a] = max(p[a], d[a]);
                return vec4<u32>(flatten(d, n), bitcast<u32>(open_at(f, a, n, m)), u32(2 * a) + select(0u, 1u, s > 0), 0u);
            }
        }
    }
    return vec4<u32>(0xffffffffu, 0u, 0xffffffffu, 0u);
}

// One workgroup, `capped` bound at the solver words: steps this tick whose
// spread hit its cap unfinished, and the step's dry, sealed and air cells.
@compute @workgroup_size(256)
fn pocket_tally(@builtin(local_invocation_index) lane: u32) {
    if lane < 3u {
        atomicStore(&pocket_counts[lane], 0u);
    }
    if lane == 0u {
        atomicStore(&pocket_first_seed, 0xffffffffu);
        atomicStore(&pocket_dry_floor, 0u);
    }
    workgroupBarrier();
    var counts = vec3<u32>(0u);
    for (var idx = lane; idx < cell_total(); idx = idx + 256u) {
        counts[min(pocket[idx], 2u)] += 1u;
        if pocket[idx] == POCKET_AIR && pocket_seed_reason(idx).z != 0xffffffffu {
            atomicMin(&pocket_first_seed, idx);
        }
        if dry_floor_hole(idx) {
            atomicAdd(&pocket_dry_floor, 1u);
        }
    }
    for (var k = 0u; k < 3u; k = k + 1u) {
        atomicAdd(&pocket_counts[k], counts[k]);
    }
    workgroupBarrier();
    if lane != 0u {
        return;
    }
    for (var k = 0u; k < 3u; k = k + 1u) {
        capped[POCKET_COUNT_WORD + k] = atomicLoad(&pocket_counts[k]);
    }
    capped[POCKET_DRY_FLOOR_WORD] = atomicLoad(&pocket_dry_floor);
    let seed = atomicLoad(&pocket_first_seed);
    capped[POCKET_SEED_WORD] = seed;
    if seed != 0xffffffffu {
        let why = pocket_seed_reason(seed);
        capped[POCKET_SEED_WORD + 1u] = why.x;
        capped[POCKET_SEED_WORD + 2u] = why.y;
        capped[POCKET_SEED_WORD + 3u] = why.z;
        if why.x != 0xffffffffu {
            capped[POCKET_SEED_WORD + 4u] = bitcast<u32>(phi[why.x]);
            capped[POCKET_SEED_WORD + 5u] = ranges[why.x].count;
        }
    }
    let unresolved = pocket_gate[POCKET_UNRESOLVED];
    if u.step_in_tick == 0 {
        capped[POCKET_WORD] = unresolved;
    } else {
        capped[POCKET_WORD] = capped[POCKET_WORD] + unresolved;
    }
}

// Open fraction of face a at record f: 0 on a box wall, which is closed in
// the solve too, else the solid's.
fn open_at(f: vec3<i32>, a: i32, n: vec3<i32>, m: vec3<i32>) -> f32 {
    if f[a] == 0 || f[a] == n[a] {
        return 0.0;
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
    let idx = c_cell_index(gid.x);
    if idx == NO_CELL {
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
    let idx = c_cell_index(gid.x);
    if idx == NO_CELL {
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

// One thread per face record, in place on `faces_rw`. A box wall face is 0
// and valid. A closed inner face (open fraction 0) keeps its
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
// A box wall face is 0, the static domain's velocity
// (FluidSimulation::_constrainVelocityFields). Weights pass through.
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
        if !face_exists(p, n, a) {
            continue;
        }
        if p[a] == 0 || p[a] == n[a] {
            out.face_velocity[a] = 0.0;
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

// Rest density: eight evenly placed particles per cell, each weighed by the
// tent kernel at the cell's centre, sum to 8.
const REST_DENSITY: f32 = 8.0;
// What a solid neighbour cell at offset d would weigh at the centre if it
// held eight evenly placed particles, as the paper samples solids with
// particles: Π over axes of 1.5 at offset 0 and 0.25 at ±1. A face neighbour
// weighs 0.5625, an edge one 0.09375, a corner one 0.015625; with the cell's
// own and its water neighbours they sum to REST_DENSITY. blub's live branch
// keeps only the face term; without the rest a seeded cell at a wall reads
// 5.5% light and a still pool creeps toward the walls.
// A cell whose centre is this many cells clear of every body has no site
// inside one; its rest sites lie within 0.44 cells of the centre.
const SOLID_SITE_REACH: f32 = 1.75;
fn solid_neighbour_density(d: vec3<i32>) -> f32 {
    let w = select(vec3<f32>(1.5), vec3<f32>(0.25), d != vec3<i32>(0));
    return w.x * w.y * w.z;
}
// The source's clamp: one projection moves a particle at most about half a
// cell.
const MAX_DENSITY_ERROR: f32 = 0.5;

// Kugelstadt et al. 2019's density source (see gpu_flip_step.rs). One thread
// per cell, the sorted particles to `cell_out`. In a water cell, ρ is the sum
// of the tent weights Π(1 − |c − q|) of the particles within a cell of its
// centre c, plus what solid rest sites would weigh: solid_neighbour_density
// for each neighbour outside the box, each body site's tent weight. Beside
// a face neighbour holding no particles ρ is at least REST_DENSITY, since a part full
// surface cell is not thin water. Out is −rate · clamp(ρ / ρ0 − 1, ±½),
// so the solve's pressure gradient moves particles out of crowded cells and
// into sparse ones; 0 outside the water.
@compute @workgroup_size(256)
fn density_source(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = c_cell_index(gid.x);
    if idx == NO_CELL {
        return;
    }
    if !(water[idx] > 0.5) {
        cell_out[idx] = 0.0;
        return;
    }
    let n = lattice();
    let m = n + vec3<i32>(1);
    let p = unflatten(idx, n);
    let centre = vec3<f32>(p) + vec3<f32>(0.5);
    let slots = u.capacity;
    var density = 0.0;
    let first = max(p - vec3<i32>(1), vec3<i32>(0));
    let last = min(p + vec3<i32>(1), n - vec3<i32>(1));
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
                    let q = (particle.position_radius.xyz - u.box_min) / u.cell_size;
                    // A particle the solid has swept over is removed by this
                    // step's move; the solid's rest sites below already count
                    // that volume, so counting it too reads as crowding.
                    if u.body_count > 0 && solid_at(q, n) < 0.0 {
                        continue;
                    }
                    let w = clamp(vec3<f32>(1.0) - abs(centre - q), vec3<f32>(0.0), vec3<f32>(1.0));
                    density = density + w.x * w.y * w.z;
                }
            }
        }
    }
    // The paper's air is a neighbour cell holding no particles: the level-set
    // mask also covers empty cells just above the surface, and their missing
    // particles would otherwise read as a thin surface layer.
    var beside_air = false;
    for (var z = -1; z <= 1; z = z + 1) {
        for (var y = -1; y <= 1; y = y + 1) {
            for (var x = -1; x <= 1; x = x + 1) {
                let d = vec3<i32>(x, y, z);
                let q = p + d;
                // Air is read on the six face neighbours only, as blub's live
                // branch does (density_projection_gather_error.comp:182-184).
                let face = abs(d.x) + abs(d.y) + abs(d.z) == 1;
                if any(q < vec3<i32>(0)) || any(q >= n) {
                    density = density + solid_neighbour_density(d);
                    continue;
                }
                var inside_body = false;
                if u.body_count > 0 {
                    let distance = solid_centre(q, m);
                    inside_body = distance < 0.0;
                    // Bodies are sampled per rest site, so a cell a body only
                    // cuts weighs its solid part; liquid_fill seeds exactly
                    // the sites outside the solid, so the two make the full
                    // lattice at rest.
                    if distance < SOLID_SITE_REACH * u.cell_size {
                        for (var k = 0; k < 8; k = k + 1) {
                            let site = vec3<f32>(q) + vec3<f32>(0.25) + 0.5 * vec3<f32>(vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1));
                            if solid_at(site, n) < 0.0 {
                                let w = clamp(vec3<f32>(1.0) - abs(centre - site), vec3<f32>(0.0), vec3<f32>(1.0));
                                density = density + w.x * w.y * w.z;
                            }
                        }
                    }
                }
                if face && !inside_body && ranges[flatten(q, n)].count == 0u {
                    beside_air = true;
                }
            }
        }
    }
    if beside_air {
        density = max(density, REST_DENSITY);
    }
    let error = clamp(density / REST_DENSITY - 1.0, -MAX_DENSITY_ERROR, MAX_DENSITY_ERROR);
    cell_out[idx] = -u.rate * error;
}

// A moved particle stays 0.2 cells inside each box wall, as the engine keeps
// its particles off its solids (`_solidBufferWidth`).
const WALL_MARGIN: f32 = 0.2;

// The CFL guard: one RK3 stage moves at most max_travel cells. A non-finite
// v stays non-finite.
fn guard(v: vec3<f32>, per_cell: f32) -> vec3<f32> {
    let cells = length(v) * per_cell;
    return select(v, v * (u.max_travel / cells), cells > u.max_travel);
}

// 1 when the guard shortens v.
fn guarded(v: vec3<f32>, per_cell: f32) -> u32 {
    return select(0u, 1u, length(v) * per_cell > u.max_travel);
}

// Exponent bits, not x != x: fast math may fold a NaN comparison away.
fn finite(v: vec3<f32>) -> bool {
    let bits = bitcast<vec3<u32>>(v) & vec3<u32>(0x7f800000u);
    return all(bits != vec3<u32>(0x7f800000u));
}

// grid: 0 the new faces, 1 `old`, 2 `spread`.
fn face_record(index: u32, grid: u32) -> FaceSample {
    if grid == 0u {
        return faces_in[index];
    }
    if grid == 1u {
        return old[index];
    }
    return spread[index];
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

// The march along a particle's move, in cells
// (_markerParticleStepDistanceFactor).
const SOLID_STEP: f32 = 0.1;
// A particle pushed out of a solid lands this many cells outside it
// (_solidBufferWidth).
const SOLID_BUFFER: f32 = 0.2;
// The farthest a push-out moves a particle, in cells (_CFLConditionNumber).
const SOLID_PUSH: f32 = 5.0;
// A bound on the solid distance's change per cell of travel: each axis of
// the trilinear distance changes at most one cell per cell.
const SOLID_SLOPE: f32 = 1.7320508;

fn solid_corner(c: vec3<i32>, n: vec3<i32>) -> f32 {
    return solid[flatten(clamp(c, vec3<i32>(0), n), n + vec3<i32>(1))];
}

// The solid distance in cells at q (cells from the box minimum), trilinear
// over the corner lattice (MeshLevelSet::trilinearInterpolate).
fn solid_at(q: vec3<f32>, n: vec3<i32>) -> f32 {
    let base = clamp(vec3<i32>(floor(q)), vec3<i32>(0), max(n - vec3<i32>(1), vec3<i32>(0)));
    let t = clamp(q - vec3<f32>(base), vec3<f32>(0.0), vec3<f32>(1.0));
    var v = 0.0;
    for (var corner = 0; corner < 8; corner = corner + 1) {
        let bit = vec3<i32>(corner & 1, (corner >> 1u) & 1, (corner >> 2u) & 1);
        let w3 = select(vec3<f32>(1.0) - t, t, bit != vec3<i32>(0));
        v = v + w3.x * w3.y * w3.z * solid_corner(base + bit, n);
    }
    return v / u.cell_size;
}

// The trilinear distance's gradient at q (Interpolation::
// trilinearInterpolateGradient): each axis's edge differences, blended
// bilinearly over the other two.
fn solid_gradient(q: vec3<f32>, n: vec3<i32>) -> vec3<f32> {
    let base = clamp(vec3<i32>(floor(q)), vec3<i32>(0), max(n - vec3<i32>(1), vec3<i32>(0)));
    let t = clamp(q - vec3<f32>(base), vec3<f32>(0.0), vec3<f32>(1.0));
    var g = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        let axes = cross_axes(a);
        var e = vec3<i32>(0);
        e[a] = 1;
        for (var k = 0; k < 4; k = k + 1) {
            var c = base;
            c[axes.x] = c[axes.x] + (k & 1);
            c[axes.y] = c[axes.y] + ((k >> 1u) & 1);
            let wx = select(1.0 - t[axes.x], t[axes.x], (k & 1) == 1);
            let wy = select(1.0 - t[axes.y], t[axes.y], ((k >> 1u) & 1) == 1);
            g[a] = g[a] + wx * wy * (solid_corner(c + e, n) - solid_corner(c, n));
        }
    }
    return g;
}

// A move from q0 to q1 (cells, both inside [edge, n − edge]) kept out of the
// solids (FluidSimulation::_resolveCollision): march in SOLID_STEP cells; at
// the first sample inside a solid, push it out along the distance's gradient
// to SOLID_BUFFER cells outside, kept inside the walls' margin, or back to
// the last sample outside when the push lands inside, moves farther than
// SOLID_PUSH or has no direction. A move that cannot reach a solid (both
// ends farther than the move's length times the distance's steepest slope)
// is kept as it is.
fn resolve_solid(q0: vec3<f32>, q1: vec3<f32>, n: vec3<i32>, edge: vec3<f32>) -> vec3<f32> {
    let travel = length(q1 - q0);
    if travel < 1e-6 || min(solid_at(q0, n), solid_at(q1, n)) > SOLID_SLOPE * travel + SOLID_STEP {
        return q1;
    }
    let steps = i32(ceil(travel / SOLID_STEP));
    let dir = (q1 - q0) / travel;
    var last = q0;
    for (var s = 0; s < steps; s = s + 1) {
        let current = select(q0 + f32(s + 1) * SOLID_STEP * dir, q1, s == steps - 1);
        let d = solid_at(current, n);
        if d < 0.0 {
            let g = solid_gradient(current, n);
            if length(g) <= 1e-6 {
                return last;
            }
            let pushed = current - (d - SOLID_BUFFER) * normalize(g);
            if length(pushed - current) > SOLID_PUSH {
                push_refused = 1u;
                return last;
            }
            if solid_at(pushed, n) < 0.0 {
                return last;
            }
            let kept = clamp(pushed, edge, vec3<f32>(n) - edge);
            if any(kept != pushed) && length(kept - pushed) > SOLID_PUSH {
                push_refused = 1u;
                return last;
            }
            if any(kept != pushed) && solid_at(kept, n) < 0.0 {
                return last;
            }
            return kept;
        }
        last = current;
    }
    return q1;
}

// An open face is a sink, as the engine's is: every wall stays solid, and a
// particle within OPEN_BOUNDARY_WIDTH cells of an open face is removed
// (FluidSimulation::_openBoundaryWidth, _removeMarkerParticles). The emptied
// band is air, so the pressure solve puts the water's surface there.
const OPEN_BOUNDARY_WIDTH: f32 = 2.0;

fn open_band(q: vec3<f32>, n: vec3<i32>) -> bool {
    for (var a = 0; a < 3; a = a + 1) {
        let low = (u.closed_faces & (1u << u32(2 * a))) == 0u;
        let high = (u.closed_faces & (1u << u32(2 * a + 1))) == 0u;
        if (low && q[a] < OPEN_BOUNDARY_WIDTH) || (high && q[a] > f32(n[a]) - OPEN_BOUNDARY_WIDTH) {
            return true;
        }
    }
    return false;
}

// One thread per particle slot, `sorted` to `particles_out`. A live particle
// (radius > 0) at q blends FLIP and PIC, flip · (v + new(q) − old(q)) +
// (1 − flip) · new(q), then moves by RK3 through the new faces (stages at ½
// and ¾ of step_dt, weights 2/9, 3/9, 4/9, each guarded), plus the density
// projection's move, kept
// WALL_MARGIN cells inside each wall. With bodies, the move is kept out of
// the solids (resolve_solid), and a particle still inside one, where a
// moving solid swept over it, is removed: radius 0
// (FluidSimulation::_removeMarkerParticles). A non-finite move or velocity
// is written as it is: the tick's stats must see it to halt the liquid.
// Radius and id are kept otherwise; unused slots pass through.
@compute @workgroup_size(256)
fn faces_to_particles(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.particles {
        return;
    }
    let particle = sorted[idx];
    var out = particle;
    let first = u.step_in_tick == 0;
    let cfl_before = select(capped[2u * idx], 0u, first);
    let push_before = select(capped[2u * idx + 1u], 0u, first);
    capped[2u * idx] = cfl_before;
    capped[2u * idx + 1u] = push_before;
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
    let s2 = sample(q0 + 0.5 * per_cell * k1, n, 0u);
    let k2 = guard(s2, per_cell);
    let s3 = sample(q0 + 0.75 * per_cell * k2, n, 0u);
    let k3 = guard(s3, per_cell);
    capped[2u * idx] = cfl_before + guarded(after, per_cell) + guarded(s2, per_cell) + guarded(s3, per_cell);
    let edge = vec3<f32>(WALL_MARGIN);
    // The density projection's move, step_dt · (spread(q) − new(q)): position
    // only, never kept as velocity. Zero rate binds `faces_in` as `spread`.
    let moved = per_cell * (sample(q0, n, 2u) - after);
    let reached = q0 + per_cell * (2.0 * k1 + 3.0 * k2 + 4.0 * k3) / 9.0 + moved;
    var q1 = select(reached, clamp(reached, edge, vec3<f32>(n) - edge), finite(reached));
    var radius = particle.position_radius.w;
    if u.body_count > 0 && finite(q1) {
        q1 = resolve_solid(q0, q1, n, edge);
        radius = select(radius, 0.0, solid_at(q1, n) < 0.0);
        capped[2u * idx + 1u] = push_before + push_refused;
    }
    if open_band(q1, n) {
        radius = 0.0;
    }
    let before = sample(q0, n, 1u);
    out.velocity = u.flip * (particle.velocity + after - before) + (1.0 - u.flip) * after;
    if u.region_count > 0 {
        // An inflow sets the velocity of the water it holds
        // (_constrainMarkerParticleVelocities); an outflow removes the water
        // that ends the move inside it.
        let x0 = particle.position_radius.xyz;
        let inflow = region_holding(x0, 2.0, true);
        if inflow >= 0 {
            out.velocity = region_velocity(inflow, x0);
        }
        if region_holding(lo + q1 * u.cell_size, 3.0, false) >= 0 {
            radius = 0.0;
        }
    }
    out.position_radius = vec4<f32>(lo + q1 * u.cell_size, radius);
    particles_out[idx] = out;
}

// Half-cell sites per axis, 2n: site j at (1/4 + j/2) cells, the fill's
// lattice and the engine's (_addNewFluidCellsThread).
fn emit_sites() -> vec3<u32> {
    return 2u * u.n;
}

fn emit_site(idx: u32) -> vec3<u32> {
    let s = emit_sites();
    return vec3<u32>(idx % s.x, (idx / s.x) % s.y, idx / (s.x * s.y));
}

// Whether a live sorted particle already sits in site j's half cell: the
// engine skips a site whose subcell of its particle mask is set
// (_addNewFluidCells), so a source tops its volume up and never stacks.
fn emit_site_taken(j: vec3<u32>) -> bool {
    let n = lattice();
    let cell = vec3<i32>(j / 2u);
    let range = ranges[flatten(cell, n)];
    for (var k = 0u; k < range.count; k = k + 1u) {
        let x = sorted[range.start + k].position_radius.xyz;
        let sub = vec3<i32>(floor(2.0 * (x - u.box_min) / u.cell_size));
        if all(sub == vec3<i32>(j)) {
            return true;
        }
    }
    return false;
}

fn emit_hash(x: u32) -> u32 {
    let s = x * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn emit_unit(x: u32) -> f32 {
    return f32(emit_hash(x) >> 8u) / 16777216.0;
}

// Site idx's particle in world space: the site, and where the inflow holds it
// deeper than a cell, moved uniformly up to emit_jitter cells each way
// (_addNewFluidCellsThread 8795-8804, _jitterMarkerParticlePosition 4409),
// keyed by the site and the substep so each substep draws afresh. The move
// stays inside the site's half cell.
fn emit_position(idx: u32, inflow: i32) -> vec3<f32> {
    let j = emit_site(idx);
    let x = fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.box_min);
    if u.emit_jitter <= 0.0 || inflow < 0 || region_distance(inflow, x) >= -u.cell_size {
        return x;
    }
    let substep = u32(u.tick_index) * 64u + u32(u.step_in_tick);
    let key = idx * 3u + substep * 2654435761u;
    let unit = vec3<f32>(emit_unit(key), emit_unit(key + 1u), emit_unit(key + 2u));
    return x + u.cell_size * u.emit_jitter * (2.0 * unit - vec3<f32>(1.0));
}

// One thread per half-cell site: 1 when an inflow emits there this substep,
// the site inside an inflow (distance at or below 0) and outside every solid
// and wall (solid distance above 0), with its half cell empty.
@compute @workgroup_size(256)
fn emit_flags(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let s = emit_sites();
    if idx >= s.x * s.y * s.z {
        return;
    }
    let j = emit_site(idx);
    let x = fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.box_min);
    let inflow = region_holding(x, 2.0, true);
    var flag = 0u;
    if inflow >= 0 && !emit_site_taken(j) {
        // The solid test is at the jittered position, as the engine's.
        let p = emit_position(idx, inflow);
        if solid_at((p - u.box_min) / u.cell_size, lattice()) > 0.0 {
            flag = 1u;
        }
    }
    emit_scan[idx] = flag;
}

// One thread per half-cell site, after the flags' inclusive scan: a flagged
// site writes a new particle at rest in its inflow's velocity to slot
// live + rank of the sorted particles, live being the sorted prefix's end.
// A rank past the pool's slots emits nothing: a full pool shows as the
// stats' live count reaching the slots, never as an error.
@compute @workgroup_size(256)
fn emit_write(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let s = emit_sites();
    if idx >= s.x * s.y * s.z {
        return;
    }
    let before = select(0u, emit_scan[idx - 1u], idx > 0u);
    if emit_scan[idx] == before {
        return;
    }
    let n = lattice();
    let last = ranges[flatten(n - vec3<i32>(1), n)];
    let slot = last.start + last.count + before;
    if slot >= u.capacity {
        return;
    }
    let j = emit_site(idx);
    let x = fma(vec3<f32>(0.25) + 0.5 * vec3<f32>(j), vec3<f32>(u.cell_size), u.box_min);
    let inflow = region_holding(x, 2.0, true);
    let p = emit_position(idx, inflow);
    // (3 / (4π · 8))^(1/3): the sphere of an eighth of a cell, as the fill's.
    emitted[slot] = FluidParticle(vec4<f32>(p, 0.31017 * u.cell_size), region_velocity(inflow, p), slot + 1u);
}

// ---- The tile table (GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 3 (The tile
// table)). Built every step from the sort's ranges, on the GPU, never read
// back; the parity and the lists live here so a replayed encode stays
// right. ----

const TILE: u32 = 8u;
// The cell passes' reach from a particle-holding cell.
const CELL_REACH: u32 = 2u;

fn tile_dims() -> vec3<u32> {
    return (u.n + vec3<u32>(TILE - 1u)) / TILE;
}

fn tile_total() -> u32 {
    let t = tile_dims();
    return t.x * t.y * t.z;
}

fn tile_parity_word() -> u32 {
    return u.ring_max + 3u;
}

// A thread of a tile list with no cell: the tile's box runs past the lattice
// (a partial edge tile).
const NO_CELL: u32 = 0xffffffffu;

// Thread `gid` of a list dispatched at 512 threads a tile: the cell at local
// index gid & 511 of the tile at list index gid >> 9, flattened, or NO_CELL.
fn list_cell(tile: u32, gid: u32) -> u32 {
    let n = lattice();
    let origin = unflatten(tile, vec3<i32>(tile_dims())) * i32(TILE);
    let cell = origin + unflatten(gid & 511u, vec3<i32>(i32(TILE)));
    if any(cell >= n) {
        return NO_CELL;
    }
    return flatten(cell, n);
}

// The cell passes run over the cell set C, the first `tile_counts[0]` tiles
// of `tiles_by_ring`, through the triple at `tile_args[0]`.
fn c_cell_index(gid: u32) -> u32 {
    return list_cell(tiles_by_ring[gid >> 9u], gid);
}

// The gather runs over C's cells too: cell p owns face record p. A record
// past the lattice (p[a] == n[a]) belongs to no cell and is a constant: wall
// faces closed, the others absent (canonical_face), written by tiles_fill.
fn c_face_index(gid: u32) -> u32 {
    let cell = c_cell_index(gid);
    if cell == NO_CELL {
        return NO_CELL;
    }
    let n = lattice();
    return flatten(unflatten(cell, n), n + vec3<i32>(1));
}

// The gather's output at record p with no particle within reach: every face
// absent (velocity 0, weight 0) except a box wall face, closed (0, weight 1).
fn canonical_face(p: vec3<i32>, n: vec3<i32>) -> FaceSample {
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    for (var a = 0; a < 3; a = a + 1) {
        if face_exists(p, n, a) && (p[a] == 0 || p[a] == n[a]) {
            out.face_weight[a] = 1.0;
        }
    }
    return out;
}

// The canonical cell values: water 0, φ 3h (particle_distance's start, kept
// where no particle is within its scan), rhs 0.
fn canonical_cell(idx: u32) {
    tile_water[idx] = 0.0;
    tile_phi[idx] = 3.0 * u.cell_size;
    tile_rhs[idx] = 0.0;
}

// One thread per face record, once per lattice: every record and cell
// canonical, so the cells the C passes never visit read as the dense passes
// would leave them.
@compute @workgroup_size(256)
fn tiles_fill(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= face_total() {
        return;
    }
    let n = lattice();
    let p = unflatten(idx, n + vec3<i32>(1));
    faces_out[idx] = canonical_face(p, n);
    if all(p < n) {
        canonical_cell(flatten(p, n));
    }
}

// 512 threads a retired tile (the triple at `tile_args[3 · (ring_max + 1)]`):
// its cells and their records back to canonical, so a tile leaving C reads
// as the dense passes would leave it. Wall records past the lattice are
// never written by the C passes and keep their fill.
@compute @workgroup_size(256)
fn tiles_retire(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = list_cell(tiles_retired[gid.x >> 9u], gid.x);
    if idx == NO_CELL {
        return;
    }
    let n = lattice();
    let p = unflatten(idx, n);
    canonical_cell(idx);
    faces_out[flatten(p, n + vec3<i32>(1))] = canonical_face(p, n);
}

// Test-only (gpu_flip_tile_tests.rs): one thread per cell, after
// tiles_retire; NaN into the water, φ and rhs of every cell of a tile with
// ring > 1 (rank >= 2), so a dense read that escapes the defined-value rule
// shows in the particles or the faces.
@compute @workgroup_size(256)
fn poison_inactive(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= cell_total() {
        return;
    }
    let n = lattice();
    let tile = flatten(unflatten(idx, n) / i32(TILE), vec3<i32>(tile_dims()));
    let half = tile_counts[tile_parity_word()] * tile_total();
    if tile_rank[half + tile] < 2u {
        return;
    }
    let nan = bitcast<f32>(0x7fc00000u);
    tile_water[idx] = nan;
    tile_phi[idx] = nan;
    tile_rhs[idx] = nan;
}

// One thread per tile, the sort's bin counts to `tile_near`: the Chebyshev
// cell distance from the tile's box to the nearest particle-holding cell,
// scanning the box grown by CELL_REACH (12³ cells at most); CELL_REACH + 1
// when none. Thread 0 flips the ring halves' parity for this step first: no
// other thread of this pass reads it, and `tiles_rings` runs after.
@compute @workgroup_size(256)
fn tiles_classify(@builtin(global_invocation_id) gid: vec3<u32>) {
    let t = gid.x;
    if t >= tile_total() {
        return;
    }
    if t == 0u {
        tile_counts[tile_parity_word()] = 1u - tile_counts[tile_parity_word()];
    }
    if u.all_tiles != 0u {
        tile_near[t] = 0u;
        return;
    }
    let n = lattice();
    let reach = i32(CELL_REACH);
    let origin = unflatten(t, vec3<i32>(tile_dims())) * i32(TILE);
    let box_last = min(origin + vec3<i32>(i32(TILE) - 1), n - vec3<i32>(1));
    let first = max(origin - vec3<i32>(reach), vec3<i32>(0));
    let last = min(box_last + vec3<i32>(reach), n - vec3<i32>(1));
    var near = CELL_REACH + 1u;
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let c = vec3<i32>(x, y, z);
                if ranges[flatten(c, n)].count == 0u {
                    continue;
                }
                // Distance from the cell to the box: 0 inside it.
                let d = max(max(origin - c, c - box_last), vec3<i32>(0));
                near = min(near, u32(max(max(d.x, d.y), d.z)));
            }
        }
    }
    tile_near[t] = near;
}

// One thread per tile, `tile_near` to the current half of `tile_rank`: the
// rank of the Chebyshev tile distance to the nearest occupied tile (near 0)
// within ring_max, else of ring_max + 1. With `all_tiles`, 0 everywhere.
@compute @workgroup_size(256)
fn tiles_rings(@builtin(global_invocation_id) gid: vec3<u32>) {
    let t = gid.x;
    let total = tile_total();
    if t >= total {
        return;
    }
    let at = tile_counts[tile_parity_word()] * total + t;
    if u.all_tiles != 0u {
        tile_rank[at] = 0u;
        return;
    }
    let dims = vec3<i32>(tile_dims());
    let p = unflatten(t, dims);
    let r = i32(u.ring_max);
    let first = max(p - vec3<i32>(r), vec3<i32>(0));
    let last = min(p + vec3<i32>(r), dims - vec3<i32>(1));
    var ring = u.ring_max + 1u;
    for (var z = first.z; z <= last.z; z = z + 1) {
        for (var y = first.y; y <= last.y; y = y + 1) {
            for (var x = first.x; x <= last.x; x = x + 1) {
                let q = vec3<i32>(x, y, z);
                if tile_near[flatten(q, dims)] != 0u {
                    continue;
                }
                let d = abs(q - p);
                ring = min(ring, u32(max(max(d.x, d.y), d.z)));
            }
        }
    }
    tile_rank[at] = rank_of(ring, tile_near[t]);
}

// The list rank of a tile: 0 in the cell set C, 1 for the rest of rings 0
// and 1, else its ring.
fn rank_of(ring: u32, near: u32) -> u32 {
    if ring <= 1u {
        return select(1u, 0u, near <= CELL_REACH);
    }
    return ring;
}

// One thread: the lists. A counting sort of the tiles by rank, stable in
// tile order: rank 0 is the cell set C (ring <= 1 and near <= CELL_REACH),
// rank 1 the rest of rings 0 and 1, rank k >= 2 ring k. The ranks' start
// offsets become their ends as the scatter advances them, and the ends are
// the counts: word 0 is |C|, word k >= 1 the tiles with ring <= k. Then the
// triples, the retired list against the previous half (exactly the tiles in
// C then and not now), and the active-fraction stats word (solver word 6,
// `capped` bound at the solver words): |C| / T³.
@compute @workgroup_size(1)
fn tiles_lists() {
    let total = tile_total();
    let r = u.ring_max;
    let parity = tile_counts[tile_parity_word()];
    let cur = parity * total;
    let prev = (1u - parity) * total;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        tile_counts[k] = 0u;
    }
    for (var t = 0u; t < total; t = t + 1u) {
        let rank = tile_rank[cur + t];
        tile_counts[rank] = tile_counts[rank] + 1u;
    }
    var start = 0u;
    for (var k = 0u; k <= r + 1u; k = k + 1u) {
        let count = tile_counts[k];
        tile_counts[k] = start;
        start = start + count;
    }
    var retired = 0u;
    for (var t = 0u; t < total; t = t + 1u) {
        let rank = tile_rank[cur + t];
        tiles_by_ring[tile_counts[rank]] = t;
        tile_counts[rank] = tile_counts[rank] + 1u;
        if tile_rank[prev + t] == 0u && rank != 0u {
            tiles_retired[retired] = t;
            retired = retired + 1u;
        }
    }
    tile_counts[r + 2u] = retired;
    for (var k = 0u; k <= r; k = k + 1u) {
        tile_args[3u * k] = 2u * tile_counts[k];
        tile_args[3u * k + 1u] = 1u;
        tile_args[3u * k + 2u] = 1u;
    }
    tile_args[3u * (r + 1u)] = 2u * retired;
    tile_args[3u * (r + 1u) + 1u] = 1u;
    tile_args[3u * (r + 1u) + 2u] = 1u;
    capped[6u] = bitcast<u32>(f32(tile_counts[0]) / f32(total));
}
