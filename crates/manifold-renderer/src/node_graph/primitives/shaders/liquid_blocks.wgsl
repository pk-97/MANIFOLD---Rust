// The block occupancy map (LIQUID_SOLVER_SEAM_DESIGN.md section 3.10): one
// u32 per 4³ block of cells, index bx + BX·(by + BY·bz). A set bit means
// "may hold", a clear bit "holds none". Twin of R/liquid/blocks.rs.
const LB_CELLS: u32 = 4u;
const LB_LIQUID: u32 = 1u;
const LB_SURFACE: u32 = 2u;
const LB_SOLID: u32 = 4u;

// Blocks per axis over `cells` cells per axis.
fn lb_blocks(cells: vec3<u32>) -> vec3<u32> {
    return (cells + vec3<u32>(LB_CELLS - 1u)) / LB_CELLS;
}

// The map index of the block holding cell `c`.
fn lb_index(c: vec3<u32>, cells: vec3<u32>) -> u32 {
    let b = c / LB_CELLS;
    let n = lb_blocks(cells);
    return b.x + n.x * (b.y + n.y * b.z);
}
