// FORCE_LATTICE (manifold-nodes-physics force_lattice.rs): trilinear reads of
// a coarse force or impulse lattice, x fastest. A force node is
// FORCE_NODE_WORDS floats: push xyz, drag rate, then drag rate × wind xyz, pad
// (`ForceSample`); an impulse node is 4 floats (xyz, pad). Pure math only: an
// atom loops k over 0..8, reads its own buffer at
// `base + corner.index * FORCE_NODE_WORDS` (impulses `corner.index * 4u`) and
// weights by `corner.weight`, so the fused rename of `buf_*` reaches every
// read. `ForceLattice::sample_forces` and `sample` (manifold-physics) are the
// CPU twins; `force_lattice_drag_step` is `ForceSample::step_velocity`.

const FORCE_NODE_WORDS: u32 = 8u;

// Word offset of the force lattice tick `tick` reads: one lattice per tick
// from `first_tick` while the field moves, one for every tick while it holds
// still.
fn force_lattice_base(tick: i32, first_tick: i32, lattices: i32, dims: vec3<u32>) -> u32 {
    let slot = clamp(tick - first_tick, 0, max(lattices - 1, 0));
    return u32(slot) * dims.x * dims.y * dims.z * FORCE_NODE_WORDS;
}

struct ForceLatticeCorner {
    index: u32,
    weight: f32,
}

fn force_lattice_corner(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, k: u32) -> ForceLatticeCorner {
    let g = (x - origin) / spacing;
    // The lattice covers the solver lattice, so g lies in [0, dims − 1]; the
    // last cell's base is dims − 2 with weight 1 on its far corner.
    let base = min(max(floor(g), vec3<f32>(0.0)), vec3<f32>(dims - vec3<u32>(2u)));
    let f = min(max(g - base, vec3<f32>(0.0)), vec3<f32>(1.0));
    let o = vec3<u32>(k & 1u, (k >> 1u) & 1u, (k >> 2u) & 1u);
    let c = vec3<u32>(base) + o;
    let w = select(vec3<f32>(1.0) - f, f, o == vec3<u32>(1u));
    var corner: ForceLatticeCorner;
    corner.index = c.x + dims.x * (c.y + dims.y * c.z);
    corner.weight = w.x * w.y * w.z;
    return corner;
}

// A velocity component `v` after `dt` under push `a`, drag rate `k` and drag
// rate × wind `kw`, integrated exactly: v e^(−k dt) + (a + kw)(1 − e^(−k dt))/k,
// stable at any rate; with k = 0 it is v + a dt.
fn force_lattice_drag_step(v: f32, a: f32, k: f32, kw: f32, dt: f32) -> f32 {
    let x = k * dt;
    var reach = dt;
    if x > 1.0e-4 {
        reach = (1.0 - exp(-x)) / k;
    } else {
        reach = dt * (1.0 - 0.5 * x);
    }
    return v * exp(-x) + (a + kw) * reach;
}
