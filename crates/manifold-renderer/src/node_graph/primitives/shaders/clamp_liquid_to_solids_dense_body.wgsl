fn body(
    idx: u32,
    count: u32,
    e_levelset: f32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    solid_nodes_x: f32,
    solid_nodes_y: f32,
    solid_nodes_z: f32,
    cell_size: f32,
) -> f32 {
    return clamp_liquid_to_solids_element(idx, count, e_levelset, center_x, center_y, center_z, size_x, size_y, size_z, nodes_x, nodes_y, nodes_z, solid_nodes_x, solid_nodes_y, solid_nodes_z, cell_size, 0u);
}
