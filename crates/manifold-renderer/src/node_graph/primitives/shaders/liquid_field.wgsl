// LIQUID_FIELD (liquid/fields.rs): trilinear reads of a liquid's coarse
// force or impulse lattice, 4 floats per node (xyz, pad), x fastest.
// Pure math only: an atom loops k over 0..8, reads its own buffer at
// `corner.index * 4u + axis` and weights by `corner.weight`, so the fused
// rename of `buf_*` reaches every read. `FieldLattice::sample` is the CPU twin.

// Word offset of the force lattice tick `tick` reads: one lattice per tick
// from `first_tick` while the field moves, one for every tick while it holds
// still. `FieldFrame::force_lattices` is the CPU side.
fn liquid_field_force_base(tick: i32, first_tick: i32, lattices: i32, dims: vec3<u32>) -> u32 {
    let slot = clamp(tick - first_tick, 0, max(lattices - 1, 0));
    return u32(slot) * dims.x * dims.y * dims.z * 4u;
}

struct LiquidFieldCorner {
    index: u32,
    weight: f32,
}

fn liquid_field_corner(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, k: u32) -> LiquidFieldCorner {
    let g = (x - origin) / spacing;
    // The lattice covers the solver lattice, so g lies in [0, dims − 1]; the
    // last cell's base is dims − 2 with weight 1 on its far corner.
    let base = min(max(floor(g), vec3<f32>(0.0)), vec3<f32>(dims - vec3<u32>(2u)));
    let f = min(max(g - base, vec3<f32>(0.0)), vec3<f32>(1.0));
    let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, (k >> 2u) & 1u);
    let c = vec3<u32>(base) + o;
    let w = select(vec3<f32>(1.0) - f, f, o == vec3<u32>(1u));
    var corner: LiquidFieldCorner;
    corner.index = c.x + dims.x * (c.y + dims.y * c.z);
    corner.weight = w.x * w.y * w.z;
    return corner;
}
