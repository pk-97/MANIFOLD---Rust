// The first Sand's grid as its tick ended, read at a world point by a
// material stepping the same tick (a Water's step, a Smoke or Fire's): its
// bulk (sand_bulk.wgsl) and the grid's velocity after forces and
// boundaries, trilinear over the eight nodes round the point (node p sits at
// p·cell). The includer composes sand_bulk.wgsl and
// `world_tiles::read_side_as("ms_")` over the Sand's table, and binds
// `sand_grid` (array<SandNode>) and `sand_bulk` (array<u32>).

// node.matter_grid_update's MatterGridNode.
struct SandNode {
    velocity_mass: vec4<f32>,
    velocity_before: vec4<f32>,
}

// The share of space the sand's bulk fills, the share its grains fill, and
// the sand's mean velocity there.
struct SandHere {
    fraction: f32,
    solid: f32,
    velocity: vec3<f32>,
}

fn sand_at(x: vec3<f32>, inv_cell: f32) -> SandHere {
    var here = SandHere(0.0, 0.0, vec3<f32>(0.0));
    let q = x * inv_cell;
    let base = vec3<i32>(floor(q));
    let f = q - floor(q);
    // A node in no held tile holds no sand. The eight nodes sit in the
    // base's tile unless it lies on the tile's far face, so a point away
    // from the sand costs one lookup.
    let first = ms_cell_at(base);
    let one_tile = all(((base + vec3<i32>(1)) >> vec3<u32>(3u)) == (base >> vec3<u32>(3u)));
    if first == ms_canon() && one_tile {
        return here;
    }
    ms_home(first);
    var momentum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, k >> 2u);
        let idx = ms_cell_at(base + vec3<i32>(o));
        if idx == ms_canon() {
            continue;
        }
        let w3 = select(1.0 - f, f, o == vec3<u32>(1u));
        let w = w3.x * w3.y * w3.z;
        let bulk = w * f32(sand_bulk[SAND_BULK_WORDS * idx]) / SAND_FIXED;
        here.fraction = here.fraction + bulk;
        here.solid = here.solid + w * f32(sand_bulk[SAND_BULK_WORDS * idx + 1u]) / SAND_FIXED;
        momentum = momentum + bulk * sand_grid[idx].velocity_mass.xyz;
    }
    if here.fraction > 0.0 {
        here.velocity = momentum / here.fraction;
    }
    return here;
}
