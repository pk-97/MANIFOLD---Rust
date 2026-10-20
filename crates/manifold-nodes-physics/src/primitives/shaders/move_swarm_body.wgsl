// node.move_swarm — fusable BUFFER body. One thread per particle advances it
// through the frame's fixed ticks under the scene's force lattice
// (semi-implicit Euler, one step per tick):
//   a = forces(x) at the tick's lattice   (force_lattice.wgsl, trilinear)
//   v += a · dt;  x += v · dt
// Each tick reads the lattice of its own tick (one per tick from first_tick,
// or one for every tick when force_lattices is 1; 0 means no forces) and its
// own length dt = tick_seconds[t], so a swarm moves the same at any frame
// rate, Speed ramps included. Dead particles (life <= 0) pass through. `in`
// is coincident (Element = Particle); `forces` and `tick_seconds` are
// gathered.
fn move_swarm_force(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, spacing, dims, k);
        let w = base + c.index * 4u;
        sum = sum + vec3<f32>(buf_forces[w], buf_forces[w + 1u], buf_forces[w + 2u]) * c.weight;
    }
    return sum;
}

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    active_count: i32,
    ticks: i32,
    first_tick: i32,
    force_lattices: i32,
    field_nodes_x: i32,
    field_nodes_y: i32,
    field_nodes_z: i32,
    field_spacing: f32,
    field_min_x: f32,
    field_min_y: f32,
    field_min_z: f32,
) -> Element {
    var p = e_in;
    if p.life <= 0.0 {
        return p;
    }
    let dims = vec3<u32>(u32(field_nodes_x), u32(field_nodes_y), u32(field_nodes_z));
    let origin = vec3<f32>(field_min_x, field_min_y, field_min_z);
    for (var t = 0; t < ticks; t = t + 1) {
        var a = vec3<f32>(0.0);
        if force_lattices > 0 {
            let base = force_lattice_base(first_tick + t, first_tick, force_lattices, dims);
            a = move_swarm_force(p.position, origin, field_spacing, dims, base);
        }
        let dt = buf_tick_seconds[u32(t)];
        p.velocity = p.velocity + a * dt;
        p.position = p.position + p.velocity * dt;
        p.age = p.age + dt;
    }
    return p;
}
