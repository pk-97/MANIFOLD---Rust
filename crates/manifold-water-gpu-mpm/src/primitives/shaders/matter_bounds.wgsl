// The Bounds box's Wall faces as placed, declared through `wgsl_includes`.
// Wall bit 2a is axis a's low face, bit 2a+1 its high face (bits −X +X −Y +Y
// −Z +Z). A node x on or past a Wall plane (x[a] <= lo[a], or >= hi[a]) stops
// moving into it: velocity is held to the wall's own speed along that axis
// (v_lo / v_hi), so a moving wall pushes the matter with it and a still wall
// (0) stops it (taichi_elements grid_bounding_box). The wall grips by Coulomb
// friction: the node's motion along the wall shrinks by `friction` times the
// speed the wall stopped, never past rest, so sand piles instead of skating.
fn matter_wall_stop(v: vec3<f32>, x: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>, walls: u32, v_lo: vec3<f32>, v_hi: vec3<f32>, friction: f32) -> vec3<f32> {
    var out = v;
    for (var a = 0u; a < 3u; a = a + 1u) {
        var stopped = 0.0;
        if (walls & (1u << (2u * a))) != 0u && x[a] <= lo[a] && out[a] < v_lo[a] {
            stopped = v_lo[a] - out[a];
            out[a] = v_lo[a];
        }
        if (walls & (2u << (2u * a))) != 0u && x[a] >= hi[a] && out[a] > v_hi[a] {
            stopped = out[a] - v_hi[a];
            out[a] = v_hi[a];
        }
        if stopped > 0.0 {
            var along = out;
            along[a] = 0.0;
            let speed = length(along);
            if speed > 0.0 {
                let kept = max(speed - friction * stopped, 0.0) / speed;
                for (var b = 0u; b < 3u; b = b + 1u) {
                    if b != a {
                        out[b] = out[b] * kept;
                    }
                }
            }
        }
    }
    return out;
}
