// The Bounds box's Wall faces as placed, declared through `wgsl_includes`.
// Wall bit 2a is axis a's low face, bit 2a+1 its high face (bits −X +X −Y +Y
// −Z +Z). A node x on or past a Wall plane (x[a] <= lo[a], or >= hi[a]) stops
// moving into it, frictionless: velocity is held to the wall's own speed
// along that axis (v_lo / v_hi), so a moving wall pushes the liquid with it
// and a still wall (0) stops it (taichi_elements grid_bounding_box).
fn matter_wall_stop(v: vec3<f32>, x: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>, walls: u32, v_lo: vec3<f32>, v_hi: vec3<f32>) -> vec3<f32> {
    var out = v;
    for (var a = 0u; a < 3u; a = a + 1u) {
        if (walls & (1u << (2u * a))) != 0u && x[a] <= lo[a] && out[a] < v_lo[a] {
            out[a] = v_lo[a];
        }
        if (walls & (2u << (2u * a))) != 0u && x[a] >= hi[a] && out[a] > v_hi[a] {
            out[a] = v_hi[a];
        }
    }
    return out;
}
