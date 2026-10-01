// node.zero_lattice — fusable BUFFER body, SOURCE. Every cell is zero; the
// wrapper's dispatch_count guard (= the lattice's cells) bounds the write.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> f32 {
    return 0.0;
}
