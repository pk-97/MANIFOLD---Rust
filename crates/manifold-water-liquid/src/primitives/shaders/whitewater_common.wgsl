// Shared by the whitewater atoms. Their grid arrays live on the liquid step's
// world tiles (`world_tiles_read.wgsl`): a cell array holds cell p's value at
// its centre, a node array the value at cell p's low corner.

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

// A scene position in whitewater cells on the world tiles: world cell p
// (centred on p·h) spans [p, p + 1), so a node array's values (cell p's low
// corner) sit at whole numbers and a cell array's at half-integers, as on a
// dense grid from its first node.
fn ww_cell_position(x: vec3<f32>, h: f32) -> vec3<f32> {
    return x / h + vec3<f32>(0.5);
}

// Stateless randomness: a 32-bit mix, and a
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

// The unit an emission count is rounded with, 0 to 1: a draw from the slot
// and the emitter's position, which moves every tick. Rounding a count down
// after adding it keeps the authored number on average, where rounding to
// the nearest drops every count under a half and doubles a rate's
// rounding at twice the Sim Rate.
fn ww_emission_unit(slot: u32, position: vec3<f32>) -> f32 {
    return ww_random(slot, bitcast<u32>(position.x), bitcast<u32>(position.y), 12u);
}
