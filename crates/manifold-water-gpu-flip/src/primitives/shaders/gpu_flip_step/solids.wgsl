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

// 512 threads a held slot, one per record, the solid corners to
// `faces_out`: each face's open fraction clamp(1 − inside) from its
// four corners (MeshLevelSet::_getFaceWeight); a face on the interface at
// every corner takes the symmetric limit ½, within the f32 rounding of a
// distance at the face's own distance from the world origin. Weight w
// of a cell's record is its open volume, 1 − the cube's inside fraction. A
// face in a wall's solid is closed like any solid's. Velocity is 0.
@compute @workgroup_size(256)
fn open_fractions(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at = list_cell(gid.x);
    let p = ts_face_coord(at);
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    for (var a = 0; a < 3; a = a + 1) {
        var centre = fma(vec3<f32>(p) + vec3<f32>(0.5), vec3<f32>(u.cell_size), u.origin);
        centre[a] = fma(f32(p[a]), u.cell_size, u.origin[a]);
        let tolerance = 8.0 * 1.1920929e-7 * max(abs(centre.x), max(abs(centre.y), abs(centre.z)));
        let axes = cross_axes(a);
        var e1 = vec3<i32>(0);
        var e2 = vec3<i32>(0);
        e1[axes.x] = 1;
        e2[axes.y] = 1;
        let c0 = solid[at];
        let c1 = solid[ts_cell_at(p + e1)];
        let c2 = solid[ts_cell_at(p + e2)];
        let c3 = solid[ts_cell_at(p + e1 + e2)];
        var inside = solid_square_inside(c0, c1, c2, c3);
        if abs(c0) <= tolerance && abs(c1) <= tolerance && abs(c2) <= tolerance && abs(c3) <= tolerance {
            inside = 0.5;
        }
        out.face_weight[a] = clamp(1.0 - inside, 0.0, 1.0);
    }
    var corners: array<f32, 8>;
    for (var k = 0; k < 8; k = k + 1) {
        corners[k] = solid[ts_cell_at(p + vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1))];
    }
    out.face_weight.w = clamp(1.0 - solid_cube_inside(corners), 0.0, 1.0);
    faces_out[at] = out;
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

// The posed bodies' signed distance at x, the smallest of them, sampled
// from each shape's lattice (liquid_collider.wgsl); far (1e30) with none.
fn bodies_distance(x: vec3<f32>) -> f32 {
    var nearest = 1e30;
    let held = min(u.body_count, u.rows);
    for (var b = 0; b < held; b = b + 1) {
        let bd = posed[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 || u32(shape_index) >= u.shapes_len {
            continue;
        }
        let sh = shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        nearest = min(nearest, liquid_shape_distance(sh.atlas_offset, dims, g, sh.origin_spacing.w, sh.scale_min.xyz));
    }
    return nearest;
}

// The solid's signed distance at x, positive in free space: the Bounds Wall
// faces and this step's posed bodies, as node.liquid_solid_distance writes
// its lattice. The solid corners hold it at the corners; emission
// tests a site with it directly, so a site needs no stored tile.
fn solid_distance(x: vec3<f32>) -> f32 {
    return min(walls_distance(x), bodies_distance(x));
}

// A body of this tick, `b` counted from its first row, posed at this step's
// end: a dynamic body by the coupled motion
// law from its tick-start row, the reaction the water put on it before this
// step and its support points; any other at its own velocity. With the pose,
// the body's response to this step's pressure while it stays on the supports
// the law leaves it on (D16), frozen for the whole solve. One thread per
// body; every solid pass of the step reads `posed` and `mobility`. Ungated: a
// zeroed row would read as shape 0 at the origin.
@compute @workgroup_size(64)
fn pose_bodies(@builtin(global_invocation_id) gid: vec3<u32>) {
    let b = i32(gid.x);
    let row = max(u.rows - u.body_count, 0) + b;
    if b >= u.body_count || row >= u.rows {
        return;
    }
    let bd = bodies[u32(row)];
    var out = bd;
    let t = adaptive_tick_seconds();
    var supports: array<vec4<f32>, 80>;
    for (var k = 0u; k < 80u; k = k + 1u) {
        supports[k] = support_vec4(80u * u32(row) + k);
    }
    let m = LiquidMobility(bd.position_inv_mass.w, bd.inv_inertia_x.xyz, bd.inv_inertia_y.xyz, bd.inv_inertia_z.xyz);
    var closed = 0u;
    var stuck = 0u;
    var unturned = 0u;
    if bd.position_inv_mass.w > 0.0 {
        let r = 8u * u32(b);
        let s = liquid_body_state(
            bd.position_inv_mass.xyz, bd.rotation, bd.linear_velocity.xyz, bd.angular_velocity.xyz, m,
            bd.accel_shape.xyz, vec3<f32>(bd.inv_inertia_x.w, bd.inv_inertia_y.w, bd.inv_inertia_z.w),
            &supports,
            vec3<f32>(reaction[r], reaction[r + 1u], reaction[r + 2u]),
            vec3<f32>(reaction[r + 4u], reaction[r + 5u], reaction[r + 6u]),
            t, u.coupled_h,
        );
        out.position_inv_mass = vec4<f32>(s.position, bd.position_inv_mass.w);
        out.rotation = s.rotation;
        out.linear_velocity = vec4<f32>(s.linear, bd.linear_velocity.w);
        out.angular_velocity = vec4<f32>(s.angular, bd.angular_velocity.w);
        closed = s.closed;
        stuck = s.stuck;
        unturned = s.unturned;
    } else {
        out.position_inv_mass = vec4<f32>(fma(bd.linear_velocity.xyz, vec3<f32>(t), bd.position_inv_mass.xyz), bd.position_inv_mass.w);
        out.rotation = liquid_turn(bd.rotation, bd.angular_velocity.xyz, t);
    }
    posed[u32(b)] = out;
    let packed = liquid_constrained_mobility(&supports, liquid_support_count(&supports), closed, stuck, unturned, m);
    let at = 6u * u32(b);
    mobility[at] = packed.m0;
    mobility[at + 1u] = packed.m1;
    mobility[at + 2u] = packed.m2;
    mobility[at + 3u] = packed.m3;
    mobility[at + 4u] = packed.m4;
    mobility[at + 5u] = packed.m5;
}

// Vec4 i of `contacts`; zero past its end (no coupled world).
fn support_vec4(i: u32) -> vec4<f32> {
    if i >= arrayLength(&contacts) {
        return vec4<f32>(0.0);
    }
    return contacts[i];
}

// The body nearest x (smallest signed distance), counted from this tick's
// first row, −1 when no body is enabled or the Wall faces are nearer: the solid
// there is the Bounds'.
fn closest_body(x: vec3<f32>) -> i32 {
    var best = -1;
    var nearest = 0.0;
    let held = min(u.body_count, u.rows);
    for (var b = 0; b < held; b = b + 1) {
        let bd = posed[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 || u32(shape_index) >= u.shapes_len {
            continue;
        }
        let sh = shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let g = liquid_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        let d = liquid_shape_distance(sh.atlas_offset, dims, g, sh.origin_spacing.w, sh.scale_min.xyz);
        if best < 0 || d < nearest {
            best = b;
            nearest = d;
        }
    }
    return select(best, -1, best >= 0 && nearest > walls_distance(x));
}

// 512 threads a held slot, one per record, `solid_faces` to
// `faces_out`. On a face
// a solid cuts (open fraction under 1): velocity is the normal part of the
// closest body's rigid velocity at the face centre, as `posed` holds it (a
// dynamic body by the coupled motion law, the same pose the solid distance
// takes), or where a Wall is nearer, that Wall's velocity; weight is the
// closest body's friction at the face's four corners,
// averaged (FluidSimulation::_getFaceFrictionU/V/W). Every other face is
// zero. Velocity w is the record's owner code (gpu_flip_bodies.wgsl): each
// face's body, counted from this tick's first row. Weight w is the known
// mask: bit a set where axis a was sampled.
@compute @workgroup_size(256)
fn solid_face_velocity(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at_record = list_cell(gid.x);
    let p = ts_face_coord(at_record);
    let open = solid_faces[at_record];
    var out = FaceSample(vec4<f32>(0.0), vec4<f32>(0.0));
    let lattice_min = u.origin;
    let h = u.cell_size;
    var code = 0.0;
    var known = 0.0;
    for (var a = 0; a < 3; a = a + 1) {
        // Only a face a solid covers is sampled (weight > 0 in
        // MeshLevelSet::_computeVelocityGridThread); solid_extrapolate
        // carries the samples out over the open faces.
        if !(open.face_weight[a] < 1.0) {
            continue;
        }
        known = known + f32(1u << u32(a));
        var centre = fma(vec3<f32>(p) + vec3<f32>(0.5), vec3<f32>(h), lattice_min);
        centre[a] = fma(f32(p[a]), h, lattice_min[a]);
        let body = closest_body(centre);
        if body >= 0 {
            let bd = posed[u32(body)];
            out.face_velocity[a] = liquid_body_velocity(bd.linear_velocity.xyz, bd.angular_velocity.xyz, bd.position_inv_mass.xyz, centre)[a];
            code = code + f32(body + 1) * f32(1u << (8u * u32(a)));
        } else {
            out.face_velocity[a] = walls_velocity(centre)[a];
        }
        let axes = cross_axes(a);
        var friction = 0.0;
        for (var k = 0; k < 4; k = k + 1) {
            var q = p;
            q[axes.x] = q[axes.x] + (k & 1);
            q[axes.y] = q[axes.y] + ((k >> 1u) & 1);
            let at = closest_body(lattice_min + vec3<f32>(q) * h);
            if at >= 0 {
                friction = friction + posed[u32(at)].linear_velocity.w;
            }
        }
        out.face_weight[a] = 0.25 * friction;
    }
    out.face_velocity.w = code;
    out.face_weight.w = known;
    faces_out[at_record] = out;
}

// Layers of the solid velocity's extrapolation
// (MeshLevelSet::_numVelocityExtrapolationLayers).
const SOLID_LAYERS: u32 = 5u;

fn solid_known(s: FaceSample, a: i32) -> bool {
    return ((u32(s.face_weight.w) >> u32(a)) & 1u) != 0u;
}

// 512 threads a held slot, one per record, `faces_in` to
// `faces_out`: one layer of the solid velocity's extrapolation (MACVelocityField::extrapolateVelocityField
// for every solid, GridUtils::extrapolateGridWithObserver). An unknown
// face with a known neighbour takes the mean of its
// known neighbours and is known from the next layer; the owner
// code goes with the first owned known neighbour, so a dynamic body's
// reaction reaches the faces its velocity reaches (the engine's
// RigidBoundaryVelocityMap::extrapolate). Run SOLID_LAYERS times.
@compute @workgroup_size(256)
fn solid_extrapolate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let at_record = list_cell(gid.x);
    let p = ts_face_coord(at_record);
    var out = faces_in[at_record];
    var known = u32(out.face_weight.w);
    var code = u32(out.face_velocity.w);
    for (var a = 0; a < 3; a = a + 1) {
        if solid_known(out, a) {
            continue;
        }
        var sum = 0.0;
        var count = 0.0;
        var owner = 0u;
        for (var b = 0; b < 3; b = b + 1) {
            for (var d = -1; d <= 1; d = d + 2) {
                var q = p;
                q[b] = p[b] + d;
                let s = faces_in[ts_face(q)];
                if solid_known(s, a) {
                    sum = sum + s.face_velocity[a];
                    count = count + 1.0;
                    let o = (u32(s.face_velocity.w) >> (8u * u32(a))) & 255u;
                    if owner == 0u {
                        owner = o;
                    }
                }
            }
        }
        if count > 0.0 {
            out.face_velocity[a] = sum / count;
            known = known | (1u << u32(a));
            code = code | (owner << (8u * u32(a)));
        }
    }
    out.face_velocity.w = f32(code);
    out.face_weight.w = f32(known);
    faces_out[at_record] = out;
}

// The solid distance at world cell p's centre, the mean of its eight
// corners (MeshLevelSet::getDistanceAtCellCenter).
fn solid_centre(p: vec3<i32>) -> f32 {
    var sum = 0.0;
    for (var k = 0; k < 8; k = k + 1) {
        sum = sum + solid[ts_cell_at(p + vec3<i32>(k & 1, (k >> 1u) & 1, (k >> 2u) & 1))];
    }
    return 0.125 * sum;
}

// A cell whose centre is inside a solid and within h/2 of the particles
// (φ under h/2) is water in the solves, its φ −h/2
// (ParticleLevelSet::postProcessSignedDistanceField). Without it a cut face
// between water and the solid's empty inside is a free surface, and the
// water beside a solid drains into it at every step.
//
// 512 threads a slot of C, in place on `cell_out`: the particles' φ.
@compute @workgroup_size(256)
fn phi_into_solids(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    let p = ts_cell_coord(idx);
    if cell_out[idx] < 0.5 * u.cell_size && solid_centre(p) < 0.0 {
        cell_out[idx] = -0.5 * u.cell_size;
    }
}

// 512 threads a slot of C, `phi` to `cell_out`: water is φ < 0,
// after `phi_into_solids`. The engine's pressure cells are its
// liquid SDF's negative cells (PressureSolver::_initialize), which reach
// 0.866h past the particles rather than stopping at the cells that hold one.
@compute @workgroup_size(256)
fn water_from_phi(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let idx = list_cell(gid.x);
    cell_out[idx] = select(0.0, 1.0, phi[idx] < 0.0);
}
