// node.mac_resolve — generated per-element BUFFER body.
//
// The input is a flat signed i32 Q20 array with six slots per padded 65³
// entry: [mass_x, momentum_x, mass_y, momentum_y, mass_z, momentum_z].
// The generated wrapper supplies `Element` from the output Channels signature.

const MAC_FIXED_SCALE: f32 = 1048576.0;

fn body(idx: u32, count: u32) -> Element {
    var velocity = vec3<f32>(0.0);
    var valid = vec3<f32>(0.0);
    let base = idx * 6u;
    for (var axis = 0u; axis < 3u; axis = axis + 1u) {
        let mass = f32(buf_accumulator[base + axis * 2u]) / MAC_FIXED_SCALE;
        let momentum = f32(buf_accumulator[base + axis * 2u + 1u]) / MAC_FIXED_SCALE;
        if (mass > 0.0 && water_finite1(mass) && water_finite1(momentum)) {
            velocity[axis] = momentum / mass;
            valid[axis] = 1.0;
        }
    }
    return Element(
        vec4<f32>(velocity, 0.0),
        vec4<f32>(valid, 0.0),
    );
}
