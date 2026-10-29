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

// The static collider meshes, from node.matter_static_solid's cache at the
// node: s = (n·(1 + row), φ), n the nearest mesh's outward normal and row its
// body. The body's row, or −1 where no mesh holds the node (φ ≥ 0).
fn matter_static_row(s: vec4<f32>) -> i32 {
    let len = length(s.xyz);
    return select(-1, i32(round(len - 1.0)), s.w < 0.0 && len > 0.5);
}

// liquid_collider_project's rule on the cached normal: relative to the body's
// velocity at the node, the inward normal part of v goes and the tangential
// part shrinks by `friction` times the speed stopped.
fn matter_static_project(v: vec3<f32>, s: vec4<f32>, v_body: vec3<f32>, friction: f32) -> vec3<f32> {
    let normal = normalize(s.xyz);
    let v_rel = v - v_body;
    let v_n = dot(v_rel, normal);
    if v_n >= 0.0 {
        return v;
    }
    let t = v_rel - v_n * normal;
    let t_len = length(t);
    let keep = max(0.0, t_len + v_n * friction);
    return v_body + select(vec3<f32>(0.0), t * (keep / t_len), t_len > 0.0);
}
