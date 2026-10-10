fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32, passes: f32, axis: i32,) -> f32 {
    return smooth_lattice_element(idx, count, nodes_x, nodes_y, nodes_z, passes, axis, 0u);
}
