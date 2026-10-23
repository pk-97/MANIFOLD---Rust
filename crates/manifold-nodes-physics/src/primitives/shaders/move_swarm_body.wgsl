// node.move_swarm — fusable BUFFER body. One thread per particle advances it
// through the frame's fixed ticks under the scene's force lattice
// (semi-implicit Euler, one step per tick):
//   a, k, k·w = forces(x) at the tick's lattice   (force_lattice.wgsl, trilinear)
//   v = drag step of v under a, k, k·w over dt
//   v += impulses(x) at the tick's impulse lattice (frames with a hit only)
//   x += v · dt
// where k is the air's drag rate and w its wind: with no air, v += a · dt.
// Each tick reads the lattice of its own tick (one per tick from first_tick,
// or one for every tick when force_lattices is 1; 0 means no forces) and its
// own length dt = tick_seconds[t], so a swarm moves the same at any frame
// rate, Speed ramps included. Dead particles (life <= 0) pass through. `in`
// is coincident (Element = Particle); `forces`, `tick_seconds` and
// `impulses` are gathered. A frame with scene hits carries one impulse lattice
// per tick from impulse_tick (zero on ticks no hit landed on), so each hit
// lands once on its own tick; impulse_tick is −1 on every frame without one.
// A particle past the lattice reads the tick's far terms instead (far_terms
// per lattice; 0 reads the lattice's edge): radial and vortex forces from
// their formulas, uniform pushes and air at their constant value. Layout and
// CPU twin: `manifold_physics::force_lattice::FarForces`.

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

// The hits' velocity change at x, from the impulse lattice starting at node
// `base` (4 floats a node).
fn move_swarm_impulse(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32) -> vec3<f32> {
    var kick = vec3<f32>(0.0);
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = force_lattice_corner(x, origin, spacing, dims, k);
        let w = (base + c.index) * 4u;
        kick = kick + vec3<f32>(buf_impulses[w], buf_impulses[w + 1u], buf_impulses[w + 2u]) * c.weight;
    }
    return kick;
}

// Floats in the far header and in one far term (FAR_HEADER and FAR_TERM
// four-float words).
const MOVE_SWARM_FAR_HEADER: u32 = 32u;
const MOVE_SWARM_FAR_TERM: u32 = 16u;

fn move_swarm_far_word(w: u32) -> vec4<f32> {
    return vec4<f32>(buf_far_forces[w], buf_far_forces[w + 1u], buf_far_forces[w + 2u], buf_far_forces[w + 3u]);
}

// A falloff curve's strength at distance d (`FalloffCurve::magnitude`):
// curve 0 Edge, 1 Inverse.
fn move_swarm_falloff(d: f32, radius: f32, power: f32, curve: f32) -> f32 {
    if curve > 0.5 {
        return pow(radius / max(d, radius), power);
    }
    if d >= radius {
        return 0.0;
    }
    return pow(1.0 - d / radius, power);
}

// A radial (kind 1) or vortex (kind 2) leaf's vector at world x, as
// `RadialField` and `VortexField` sample it. `head` is (kind, curve, radius,
// falloff).
fn move_swarm_far_leaf(x: vec3<f32>, head: vec4<f32>, center: vec3<f32>, axis: vec3<f32>) -> vec3<f32> {
    let vortex = head.x > 1.5;
    var arm = x - center;
    if vortex {
        arm = arm - axis * dot(arm, axis);
    }
    let d = length(arm);
    if d <= 1.1920929e-7 {
        return vec3<f32>(0.0);
    }
    let m = move_swarm_falloff(d, head.z, head.w, head.y) / d;
    if vortex {
        return cross(axis, arm) * m;
    }
    return arm * m;
}

// The force past the lattice at x (in the swarm's frame) from lattice slot
// `slot`'s `terms` far terms: summed in the world, turned back into the frame.
fn move_swarm_far(x: vec3<f32>, slot: u32, terms: u32) -> MoveSwarmForce {
    let world = move_swarm_far_word(16u).xyz + move_swarm_far_word(4u).xyz * x.x + move_swarm_far_word(8u).xyz * x.y
        + move_swarm_far_word(12u).xyz * x.z;
    var push = vec3<f32>(0.0);
    var drag = 0.0;
    var drag_wind = vec3<f32>(0.0);
    for (var i = 0u; i < terms; i = i + 1u) {
        let w = MOVE_SWARM_FAR_HEADER + (slot * terms + i) * MOVE_SWARM_FAR_TERM;
        let head = move_swarm_far_word(w);
        let a = move_swarm_far_word(w + 4u);
        let b = move_swarm_far_word(w + 8u);
        drag = drag + b.w;
        if head.x < 0.5 {
            push = push + a.xyz;
            drag_wind = drag_wind + b.xyz;
        } else {
            let f = move_swarm_far_leaf(world, head, a.xyz, b.xyz);
            push = push + f * a.w;
            drag_wind = drag_wind + f * buf_far_forces[w + 12u];
        }
    }
    let r0 = move_swarm_far_word(20u).xyz;
    let r1 = move_swarm_far_word(24u).xyz;
    let r2 = move_swarm_far_word(28u).xyz;
    var f: MoveSwarmForce;
    f.push_drag = vec4<f32>(dot(r0, push), dot(r1, push), dot(r2, push), drag);
    f.drag_wind = vec3<f32>(dot(r0, drag_wind), dot(r1, drag_wind), dot(r2, drag_wind));
    return f;
}

// Whether x lies past the lattice's last node on any axis.
fn move_swarm_past_lattice(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>) -> bool {
    let g = (x - origin) / spacing;
    return any(g < vec3<f32>(0.0)) || any(g > vec3<f32>(dims - vec3<u32>(1u)));
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
    far_terms: i32,
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
            var f: MoveSwarmForce;
            if far_terms > 0 && move_swarm_past_lattice(p.position, origin, field_spacing, dims) {
                f = move_swarm_far(p.position, u32(clamp(t, 0, force_lattices - 1)), u32(far_terms));
            } else {
                f = move_swarm_force(p.position, origin, field_spacing, dims, base);
            }
            for (var axis = 0; axis < 3; axis = axis + 1) {
                p.velocity[axis] = force_lattice_drag_step(p.velocity[axis], f.push_drag[axis], f.push_drag.w, f.drag_wind[axis], dt);
            }
        }
        let slot = first_tick + t - impulse_tick;
        if impulse_tick >= 0 && slot >= 0 && slot < ticks {
            let base = u32(slot) * dims.x * dims.y * dims.z;
            p.velocity = p.velocity + move_swarm_impulse(p.position, origin, field_spacing, dims, base);
        }
        p.position = p.position + p.velocity * dt;
        p.age = p.age + dt;
    }
    return p;
}
