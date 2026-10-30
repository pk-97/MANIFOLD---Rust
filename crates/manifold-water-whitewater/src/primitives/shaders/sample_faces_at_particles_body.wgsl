// node.sample_faces_at_particles — fusable BUFFER body, COINCIDENT
// particles, GATHER faces and tiles. A live particle's velocity becomes
// FLIP's MAC trilinear of the step's pooled faces at its position
// (macvelocityfield.cpp:635): each component from the eight faces of its
// staggered stencil, and 0 on every axis for a particle whose cell lies in no
// stored tile. Position, radius and id pass through; a slot with radius 0
// passes whole. Face f of an axis is cell f's low face, the `faces` record of
// world cell f read through the world-tile table (`tiles`, `pool_tiles`
// tiles); a face in an absent tile reads the canonical record, and a face
// whose weight is not positive reads 0.
//
// Ported from FLIP Fluids macvelocityfield.cpp (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }
fn body(
    idx: u32,
    count: u32,
    e_particles: Element,
    cell_size: f32,
    pool_tiles: f32,
) -> Element {
    ww_pool_tiles = u32(pool_tiles);
    if !(e_particles.position_radius.w > 0.0) {
        return e_particles;
    }
    let still = Element(e_particles.position_radius, vec3<f32>(0.0), e_particles.id);
    let q = ww_cell_position(e_particles.position_radius.xyz, cell_size);
    let cell = vec3<i32>(floor(q));
    ts_home(ts_cell_at(cell));
    if !ts_cell_stored(cell) {
        return still;
    }
    var v = vec3<f32>(0.0);
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let s = lf_stencil(q, axis);
        let lower = floor(s);
        let f = s - lower;
        let base = vec3<i32>(lower);
        var sum = 0.0;
        for (var corner = 0u; corner < 8u; corner = corner + 1u) {
            let face = buf_faces[ts_face(base + vec3<i32>(ww_corner(corner)))];
            sum = sum + ww_corner_weight(f, corner) * lf_face_velocity(face.face_velocity, face.face_weight, axis);
        }
        v[axis] = sum;
    }
    return Element(e_particles.position_radius, v, e_particles.id);
}
