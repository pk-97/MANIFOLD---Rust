// Test-only pre-brick dense reference, main 4aab34f86.
// node.count_surface_triangles — fusable BUFFER body, GATHER. One thread per
// lattice cell: the marching-cubes case of its eight corners and that case's
// triangle count (GPU_FLUID_SURFACE_DESIGN.md D16). Slots past the last cell
// count 0. `levelset` (f32) is gathered through `buf_levelset`; the tables
// come from marching_cubes_common.wgsl.

fn body(idx: u32, count: u32, nodes_x: f32, nodes_y: f32, nodes_z: f32) -> u32 {
    // Fewer than two nodes on an axis: no lattice yet, no triangles.
    if min(min(nodes_x, nodes_y), nodes_z) < 2.0 {
        return 0u;
    }
    let nodes = vec3<u32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let cells = nodes - vec3<u32>(1u);
    if idx >= cells.x * cells.y * cells.z {
        return 0u;
    }
    let cell = vec3<u32>(idx % cells.x, (idx / cells.x) % cells.y, idx / (cells.x * cells.y));
    return MC_TRIANGLE_COUNT[mc_case(cell, nodes)];
}
