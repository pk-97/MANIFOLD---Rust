// node.cells_with_particles — fusable BUFFER body. One thread per bin: 1
// where the sort put at least one particle in it, else 0.

fn body(idx: u32, count: u32, e_cell_ranges: Element) -> f32 {
    return select(0.0, 1.0, e_cell_ranges.count > 0u);
}
