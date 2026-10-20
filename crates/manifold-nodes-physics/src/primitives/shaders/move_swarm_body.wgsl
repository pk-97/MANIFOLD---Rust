// node.move_swarm — fusable BUFFER body. One thread per particle advances it
// through the frame's fixed ticks under the scene's force lattice
// (semi-implicit Euler, one step per tick):
//   a, k, k·w = forces(x) at the tick's lattice   (force_lattice.wgsl, trilinear)
//   v = drag step of v under a, k, k·w over dt
//   v += impulses(x)                              (on impulse_tick only)
//   x += v · dt
// where k is the air's drag rate and w its wind: with no air, v += a · dt.
// Each tick reads the lattice of its own tick (one per tick from first_tick,
// or one for every tick when force_lattices is 1; 0 means no forces) and its
// own length dt = tick_seconds[t], so a swarm moves the same at any frame
// rate, Speed ramps included. Dead particles (life <= 0) pass through. `in`
// is coincident (Element = Particle); `forces`, `tick_seconds` and
// `impulses` are gathered. A scene hit lands once: impulse_tick is −1 on
// every frame without one.

// The tick's force at x: push and drag rate, then drag rate × wind.
struct MoveSwarmForce {
    push_drag: vec4<f32>,
    drag_wind: vec3<f32>,
}

fn move_swarm_force(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32) -> MoveSwarmForce {
    var f: MoveSwarmForce;
    f.push_drag = vec4<f32>(0.0);
    f.drag_wind = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, spacing, dims, k);
        let w = base + c.index * FORCE_NODE_WORDS;
        f.push_drag = f.push_drag + vec4<f32>(buf_forces[w], buf_forces[w + 1u], buf_forces[w + 2u], buf_forces[w + 3u]) * c.weight;
        f.drag_wind = f.drag_wind + vec3<f32>(buf_forces[w + 4u], buf_forces[w + 5u], buf_forces[w + 6u]) * c.weight;
    }
    return f;
}

// The hit's velocity change at x, from the impulse lattice (4 floats a node).
fn move_swarm_impulse(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>) -> vec3<f32> {
    var kick = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, spacing, dims, k);
        let w = c.index * 4u;
        kick = kick + vec3<f32>(buf_impulses[w], buf_impulses[w + 1u], buf_impulses[w + 2u]) * c.weight;
    }
    return kick;
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
    impulse_tick: i32,
) -> Element {
    var p = e_in;
    if p.life <= 0.0 {
        return p;
    }
    let dims = vec3<u32>(u32(field_nodes_x), u32(field_nodes_y), u32(field_nodes_z));
    let origin = vec3<f32>(field_min_x, field_min_y, field_min_z);
    for (var t = 0; t < ticks; t = t + 1) {
        let dt = buf_tick_seconds[u32(t)];
        if force_lattices > 0 {
            let base = force_lattice_base(first_tick + t, first_tick, force_lattices, dims);
            let f = move_swarm_force(p.position, origin, field_spacing, dims, base);
            for (var axis = 0; axis < 3; axis = axis + 1) {
                p.velocity[axis] = force_lattice_drag_step(p.velocity[axis], f.push_drag[axis], f.push_drag.w, f.drag_wind[axis], dt);
            }
        }
        if first_tick + t == impulse_tick {
            p.velocity = p.velocity + move_swarm_impulse(p.position, origin, field_spacing, dims);
        }
        p.position = p.position + p.velocity * dt;
        p.age = p.age + dt;
    }
    return p;
}
