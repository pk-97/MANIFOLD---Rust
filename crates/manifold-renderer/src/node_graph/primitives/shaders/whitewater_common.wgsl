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

// Whether cell c lies in the grid; FLIP reads 0 (or solid) outside it.
fn ww_in_grid(c: vec3<i32>, cells: vec3<u32>) -> bool {
    return all(c >= vec3<i32>(0)) && all(c < vec3<i32>(cells));
}

// A scene position in grid cells from the grid's first node, the grid being
// the box `center` ± `size`/2 over `cells`.
fn ww_grid_position(p: vec3<f32>, center: vec3<f32>, size: vec3<f32>, cells: vec3<u32>) -> vec3<f32> {
    return (p - (center - 0.5 * size)) * vec3<f32>(cells) / size;
}

// Stateless randomness (GPU_WHITEWATER_DESIGN.md D10): a 32-bit mix, and a
// uniform number in [0, 1) per (slot, seed, epoch, stream).
fn ww_hash(x: u32) -> u32 {
    var h = x;
    h = h ^ (h >> 16u);
    h = h * 0x7feb352du;
    h = h ^ (h >> 15u);
    h = h * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return h;
}

fn ww_random(slot: u32, seed: u32, epoch: u32, stream: u32) -> f32 {
    let h = ww_hash(slot + ww_hash(seed + ww_hash(epoch * 16u + stream)));
    return f32(h >> 8u) * (1.0 / 16777216.0);
}
