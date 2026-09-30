// Shared by the whitewater grid atoms (GPU_WHITEWATER_DESIGN.md section 3.3).
// The grid is the solid lattice read as cells: cells = nodes − 1 a side, cell
// c at c.x + cells.x·(c.y + cells.y·c.z), its corners the solid nodes c..c+1.

fn ww_cell(idx: u32, cells: vec3<u32>) -> vec3<u32> {
    return vec3<u32>(idx % cells.x, (idx / cells.x) % cells.y, idx / (cells.x * cells.y));
}

fn ww_cell_index(c: vec3<u32>, cells: vec3<u32>) -> u32 {
    return c.x + cells.x * (c.y + cells.y * c.z);
}

fn ww_on_border(c: vec3<u32>, cells: vec3<u32>) -> bool {
    return any(c == vec3<u32>(0u)) || any(c + vec3<u32>(1u) == cells);
}

// Corner `corner` of a cell: bit 0 x, bit 1 y, bit 2 z.
fn ww_corner(corner: u32) -> vec3<u32> {
    return vec3<u32>(corner & 1u, (corner >> 1u) & 1u, (corner >> 2u) & 1u);
}

// Trilinear weight of `corner` at `f` in [0, 1]³.
fn ww_corner_weight(f: vec3<f32>, corner: u32) -> f32 {
    let o = ww_corner(corner);
    return select(1.0 - f.x, f.x, o.x == 1u)
        * select(1.0 - f.y, f.y, o.y == 1u)
        * select(1.0 - f.z, f.z, o.z == 1u);
}

// The six face neighbours, in the order every atom visits them.
fn ww_face_step(n: u32) -> vec3<i32> {
    var step = vec3<i32>(0);
    step[n / 2u] = select(-1, 1, (n & 1u) == 1u);
    return step;
}
