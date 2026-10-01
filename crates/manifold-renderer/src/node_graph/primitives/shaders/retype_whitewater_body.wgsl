// node.retype_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// distance, cells and faces. FLIP's _updateDiffuseParticleTypes
// (diffuseparticlesimulation.cpp:2033) after each advect, with FLIP's
// defaults: the same rule node.whitewater_type gives a fresh particle
// (spray outside the boundary box; else foam within a cell of the surface,
// bubble deeper, spray higher; foam or spray away from air becomes bubble),
// except that foam turning bubble stays foam until it sinks a further cell
// (_foamBufferWidth). A bubble that turns foam or spray takes the liquid
// velocity at its position (FLIP's MAC trilinear, 0 outside the grid).
// Slots with lifetime <= 0 or an unknown type pass whole.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance, _foamLayerOffset and _foamBufferWidth,
// cells.
const RT_FOAM_DEPTH: f32 = 1.0;
const RT_FOAM_OFFSET: f32 = 0.0;
const RT_FOAM_BUFFER: f32 = 1.0;
// FLIP's boundary box inset, cells, and its 1e-6 m epsilon halved.
const RT_BOX_INSET: f32 = 1.625;
const RT_BOX_EPSILON: f32 = 0.5e-6;

fn rt_distance(c: vec3<i32>, cells: vec3<u32>) -> f32 {
    if !ww_in_grid(c, cells) {
        return 0.0;
    }
    return buf_distance[ww_cell_index(vec3<u32>(c), cells)];
}

fn rt_borders_air(c: vec3<i32>, cells: vec3<u32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let n = c + vec3<i32>(dx, dy, dz);
                if (dx == 0 && dy == 0 && dz == 0) || !ww_in_grid(n, cells) {
                    continue;
                }
                if buf_cells[ww_cell_index(vec3<u32>(n), cells)] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

fn rt_face_len(axis: u32) -> u32 {
    if axis == 0u {
        return arrayLength(&buf_face_u);
    }
    if axis == 1u {
        return arrayLength(&buf_face_v);
    }
    return arrayLength(&buf_face_w);
}

fn rt_face(axis: u32, i: u32) -> f32 {
    if axis == 0u {
        return buf_face_u[i];
    }
    if axis == 1u {
        return buf_face_v[i];
    }
    return buf_face_w[i];
}

// FLIP's MAC trilinear at grid position q.
fn rt_velocity(q: vec3<f32>, cells: vec3<u32>, face_cells: vec3<u32>) -> vec3<f32> {
    if any(q < vec3<f32>(0.0)) || any(q >= vec3<f32>(cells)) {
        return vec3<f32>(0.0);
    }
    let pad = lf_pad(cells, face_cells);
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        let len = rt_face_len(axis);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let i = lf_face_index(base + vec3<i32>(ww_corner(corner)), axis, pad, face_cells);
            if i != LF_NONE && i < len {
                sum = sum + ww_corner_weight(f, corner) * rt_face(axis, i);
            }
        }
        v[axis] = sum;
    }
    return v;
}

fn rt_kind(q: vec3<f32>, old: u32, h: f32, cells: vec3<u32>) -> u32 {
    let lo = vec3<f32>(RT_BOX_INSET + RT_BOX_EPSILON / h);
    let hi = vec3<f32>(cells) - lo;
    if any(q < lo) || any(q >= hi) {
        return 2u;
    }
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * rt_distance(base + vec3<i32>(ww_corner(corner)), cells);
    }
    let depth = RT_FOAM_DEPTH * h;
    let offset = RT_FOAM_OFFSET * h;
    var kind = 2u;
    if d > -depth + offset && d < depth + offset {
        kind = 1u;
    } else if d < -depth + offset {
        kind = 0u;
    }
    if old == 1u && kind == 0u && d > -depth - RT_FOAM_BUFFER * h + offset {
        kind = 1u;
    }
    if kind != 0u && !rt_borders_air(vec3<i32>(floor(q)), cells) {
        kind = 0u;
    }
    return kind;
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    face_cells_x: f32,
    face_cells_y: f32,
    face_cells_z: f32,
) -> Element {
    var out = e_pool;
    if !(e_pool.position_lifetime.w > 0.0) || e_pool.kind > 2u {
        return out;
    }
    let nodes = vec3<u32>(max(vec3<f32>(nodes_x, nodes_y, nodes_z), vec3<f32>(0.0)));
    let face_cells = vec3<u32>(max(round(vec3<f32>(face_cells_x, face_cells_y, face_cells_z)), vec3<f32>(0.0)));
    if any(nodes < vec3<u32>(3u)) || any(face_cells == vec3<u32>(0u)) {
        return out;
    }
    let cells = nodes - vec3<u32>(1u);
    let total = cells.x * cells.y * cells.z;
    if any(face_cells > cells) || total > arrayLength(&buf_distance) || total > arrayLength(&buf_cells) {
        return out;
    }
    let size = vec3<f32>(size_x, size_y, size_z);
    let h = size.x / f32(cells.x);
    let q = ww_grid_position(e_pool.position_lifetime.xyz, vec3<f32>(center_x, center_y, center_z), size, cells);
    let kind = rt_kind(q, e_pool.kind, h, cells);
    if e_pool.kind == 0u && kind != 0u {
        out.velocity = rt_velocity(q, cells, face_cells);
    }
    out.kind = kind;
    return out;
}
