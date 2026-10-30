var<private> ww_pool_tiles: u32;
fn ts_pool_tiles() -> u32 { return ww_pool_tiles; }
fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// FLIP Fluids turbulencefield.cpp:100-171 (MIT); THIRD_PARTY_NOTICES.md.
// One thread per pool element of the liquid step's world tiles (`buf_tiles`,
// `pool_tiles` tiles). Face f of axis a is cell f's low face, the pooled
// record `ts_face(f)`, and reads 0 where its weight is not positive; a cell in
// a tile the table does not hold reads the canonical record, no velocity.
// The asymmetric i-2 through i+1 window is intentional. Air, a free slot and
// the canonical element are zero.

fn tf_velocity(c: vec3<i32>) -> vec3<f32> {
    let own = buf_faces[ts_face(c)];
    let x = buf_faces[ts_face(c + vec3<i32>(1, 0, 0))];
    let y = buf_faces[ts_face(c + vec3<i32>(0, 1, 0))];
    let z = buf_faces[ts_face(c + vec3<i32>(0, 0, 1))];
    return 0.5 * (
        vec3<f32>(
            lf_face_velocity(own.face_velocity, own.face_weight, 0u),
            lf_face_velocity(own.face_velocity, own.face_weight, 1u),
            lf_face_velocity(own.face_velocity, own.face_weight, 2u),
        ) + vec3<f32>(
            lf_face_velocity(x.face_velocity, x.face_weight, 0u),
            lf_face_velocity(y.face_velocity, y.face_weight, 1u),
            lf_face_velocity(z.face_velocity, z.face_weight, 2u),
        )
    );
}

fn body(idx: u32, count: u32, cell_size: f32, pool_tiles: f32) -> f32 {
    ww_pool_tiles = u32(pool_tiles);
    if idx >= ts_canon() { return 0.0; }
    if ts_slot_entry(idx >> 9u) == WT_NONE { return 0.0; }
    if buf_distance[idx] >= 0.0 { return 0.0; }
    ts_home(idx);
    let c = ts_cell_coord(idx);
    let vi = tf_velocity(c);
    var t = 0.0;
    for (var z = c.z - 2; z < c.z + 2; z++) {
        for (var y = c.y - 2; y < c.y + 2; y++) {
            for (var x = c.x - 2; x < c.x + 2; x++) {
                let n = vec3<i32>(x, y, z);
                let dv = vi - tf_velocity(n);
                let speed = length(dv);
                if speed < 1e-5 { continue; }
                let delta = vec3<f32>(c - n) * cell_size;
                let r = length(delta);
                t += speed * (1.0 - dot(dv / speed, delta / r)) * (1.0 - r / (sqrt(12.0) * cell_size));
            }
        }
    }
    return t;
}
