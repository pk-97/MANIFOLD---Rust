// node.cells_with_particles — fusable BUFFER body. One thread per lattice
// cell (the sort's bin with the same index): 1 where the sort put at least
// one particle in it, else 0; 0 past the lattice.

fn body(idx: u32, count: u32, e_cell_ranges: Element, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    let cells = u32(nodes_x) * u32(nodes_y) * u32(nodes_z);
    return select(0.0, 1.0, idx < cells && e_cell_ranges.count > 0u);
}
