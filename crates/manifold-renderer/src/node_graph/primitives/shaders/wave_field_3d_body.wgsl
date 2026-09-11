// node.wave_field_3d — point-sampled travelling sine field.
const TAU: f32 = 6.283185307179586;

fn body(
    idx: u32,
    count: u32,
    e_positions: Element,
    frequency: f32,
    phase: f32,
    direction_x: f32,
    direction_y: f32,
    direction_z: f32,
) -> f32 {
    let spatial = e_positions.x * direction_x
        + e_positions.y * direction_y
        + e_positions.z * direction_z;
    let wrapped_phase = phase - floor(phase);
    return sin(TAU * (spatial * frequency - wrapped_phase));
}
