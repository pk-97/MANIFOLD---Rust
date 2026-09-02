// node.scene_array — fusable BUFFER body (freeze section 12, buffer domain), SOURCE.
// Fill an Array<InstanceTransform> with identity TRS translated i * cell_size along axis.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the output element written to
// buf_out[idx]. The codegen synthesizes
//   struct Element { pos_scale: vec4<f32>, rot: vec4<f32> }
// from InstanceTransform's Channels signature. `dispatch_count` (= the OUTPUT
// capacity) is the wrapper guard.
//
// Axis encoding: 0=+X, 1=-X, 2=+Y, 3=-Y, 4=+Z, 5=-Z.
// Int params arrive as i32, Enum as u32, Float as f32.

fn body(
    idx: u32,
    count: u32,
    count_param: i32,
    axis: u32,
    cell_size: f32,
) -> Element {
    let t = f32(idx) * cell_size;
    var pos = vec3<f32>(0.0, 0.0, 0.0);
    if axis == 0u { pos.x = t; }
    else if axis == 1u { pos.x = -t; }
    else if axis == 2u { pos.y = t; }
    else if axis == 3u { pos.y = -t; }
    else if axis == 4u { pos.z = t; }
    else { pos.z = -t; }
    // Identity rotation, unit scale.
    return Element(vec4<f32>(pos, 1.0), vec4<f32>(0.0, 0.0, 0.0, 0.0));
}
