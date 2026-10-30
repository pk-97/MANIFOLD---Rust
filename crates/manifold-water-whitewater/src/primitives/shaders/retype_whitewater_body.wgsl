var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.retype_whitewater — fusable BUFFER body, COINCIDENT pool, GATHER
// distance, cells, faces and the world-tile table. FLIP's
// _updateDiffuseParticleTypes (diffuseparticlesimulation.cpp:2033) after each
// advect, with FLIP's defaults: the same rule node.whitewater_type gives a
// fresh particle (foam within a cell of the surface, bubble deeper, spray
// higher; foam or spray away from air becomes bubble), except that foam
// turning bubble stays foam until it sinks a further cell (_foamBufferWidth).
// A bubble that turns foam or spray takes the liquid velocity at its position
// (FLIP's MAC trilinear over the step's pooled faces, 0 in a tile the table
// does not hold). The distance and cells are cell arrays on the same table; a
// neighbour in a tile it does not hold reads the canonical element, air.
// There is no boundary box: a wall is a solid.
// Dust (kind 4) and empty slots (kind 3) pass whole; a dead particle is still retyped, as FLIP retypes it before removal.
//
// Ported from FLIP Fluids diffuseparticlesimulation.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// FLIP's _maxFoamToSurfaceDistance, _foamLayerOffset and _foamBufferWidth,
// cells.
const RT_FOAM_DEPTH: f32 = 1.0;
const RT_FOAM_OFFSET: f32 = 0.0;
const RT_FOAM_BUFFER: f32 = 1.0;

fn rt_distance(c: vec3<i32>) -> f32 {
    return buf_distance[ts_cell_at(c)];
}

fn rt_borders_air(c: vec3<i32>) -> bool {
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                if dx == 0 && dy == 0 && dz == 0 {
                    continue;
                }
                if buf_cells[ts_cell_at(c + vec3<i32>(dx, dy, dz))] == 0u {
                    return true;
                }
            }
        }
    }
    return false;
}

// Component `axis` of the face of cell c.
fn rt_face(axis: u32, c: vec3<i32>) -> f32 {
    let s = buf_faces[ts_face(c)];
    return lf_face_velocity(s.face_velocity, s.face_weight, axis);
}

// FLIP's MAC trilinear at grid position q.
fn rt_velocity(q: vec3<f32>) -> vec3<f32> {
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            sum = sum + ww_corner_weight(f, corner) * rt_face(axis, base + vec3<i32>(ww_corner(corner)));
        }
        v[axis] = sum;
    }
    return v;
}

fn rt_kind(q: vec3<f32>, old: u32, h: f32) -> u32 {
    let s = q - vec3<f32>(0.5);
    let lower = floor(s);
    let f = s - lower;
    let base = vec3<i32>(lower);
    var d = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        d = d + ww_corner_weight(f, corner) * rt_distance(base + vec3<i32>(ww_corner(corner)));
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
    if kind != 0u && !rt_borders_air(vec3<i32>(floor(q))) {
        kind = 0u;
    }
    return kind;
}

fn body(
    idx: u32,
    count: u32,
    e_pool: Element,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    var out = e_pool;
    if e_pool.kind > 2u {
        return out;
    }
    let q = ww_cell_position(e_pool.position_lifetime.xyz, cell_size);
    ts_home(ts_cell_at(vec3<i32>(floor(q))));
    let kind = rt_kind(q, e_pool.kind, cell_size);
    if e_pool.kind == 0u && kind != 0u {
        out.velocity = rt_velocity(q);
    }
    out.kind = kind;
    return out;
}
