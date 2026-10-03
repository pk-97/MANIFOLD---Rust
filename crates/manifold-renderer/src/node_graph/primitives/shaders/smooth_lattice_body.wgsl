fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, passes: f32, axis: i32, brick_pass: u32) -> f32 {
    return smooth_lattice_element(idx, count, nodes_x, nodes_y, nodes_z, passes, axis, brick_pass);
}

fn liquid_brick_index(invocation: u32) -> u32 {
    let dims = vec3<u32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z));
    return liquid_brick_select(invocation, dims, params.brick_pass);
}
