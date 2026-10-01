// node.face_gravity — fusable BUFFER body. One thread per padded cell of the
// face grid: each face that exists gains
//   v += step_dt · (g + forces(x))[a]               (forces from tick_index's lattice)
//   v += impulses(x)[a]                             (step 0 of impulse_tick)
// along its normal a, x the face's centre: the lattice minimum plus
// cell_size · (p + ½) with the normal axis at p. forces and impulses are
// trilinear reads of the domain's coarse field lattices (liquid_field.wgsl;
// origin the lattice minimum, 4 floats a node). A box wall face (index 0 or
// nodes along its axis) then keeps only the part leaving the wall, the rule
// node.particles_to_faces applies: water pulled off the lid leaves it, water
// pushed into the floor stops. The pressure solve takes a wall face's
// velocity as given, so a leaving wall face asks no suction of the water.
// Weights pass through; faces past the lattice give zeros. Every multiply-add
// is an explicit fma: under fast math the compiler contracts a bare a * b + c
// differently in the standalone and the fused kernel, which made fused and
// unfused differ by an ulp.

fn face_gravity_force(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, base: u32, axis: u32) -> f32 {
    var sum = 0.0;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = liquid_field_corner(x, origin, spacing, dims, k);
        sum = fma(buf_forces[base + c.index * 4u + axis], c.weight, sum);
    }
    return sum;
}

fn face_gravity_impulse(x: vec3<f32>, origin: vec3<f32>, spacing: f32, dims: vec3<u32>, axis: u32) -> f32 {
    var sum = 0.0;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let c = liquid_field_corner(x, origin, spacing, dims, k);
        sum = fma(buf_impulses[c.index * 4u + axis], c.weight, sum);
    }
    return sum;
}

fn body(
    idx: u32,
    count: u32,
    e_faces: Element,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    step_dt: f32,
    cell_size: f32,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    tick_index: i32,
    substep_in_tick: i32,
    field_nodes_x: i32,
    field_nodes_y: i32,
    field_nodes_z: i32,
    field_spacing: f32,
    force_lattices: i32,
    impulse_tick: i32,
    first_tick: i32,
) -> Element {
    var out = Element(vec4<f32>(0.0), vec4<f32>(0.0));
    let n = vec3<i32>(vec3<f32>(nodes_x, nodes_y, nodes_z));
    let m = n + vec3<i32>(1);
    if idx >= u32(m.x) * u32(m.y) * u32(m.z) {
        return out;
    }
    let p = vec3<i32>(
        i32(idx % u32(m.x)),
        i32((idx / u32(m.x)) % u32(m.y)),
        i32(idx / (u32(m.x) * u32(m.y))),
    );
    let gravity = vec3<f32>(gravity_x, gravity_y, gravity_z);
    let origin = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let field_dims = vec3<u32>(u32(field_nodes_x), u32(field_nodes_y), u32(field_nodes_z));
    var force_base = 0u;
    if force_lattices > 0 {
        force_base = liquid_field_force_base(tick_index, first_tick, force_lattices, field_dims);
    }
    let impulse = tick_index == impulse_tick && substep_in_tick == 0;
    for (var a = 0; a < 3; a = a + 1) {
        var other = p;
        other[a] = 0;
        if !all(other < n) {
            continue;
        }
        out.face_weight[a] = e_faces.face_weight[a];
        var centre = vec3<f32>(p) + vec3<f32>(0.5);
        centre[a] = f32(p[a]);
        let x = fma(centre, vec3<f32>(cell_size), origin);
        var accel = gravity[a];
        if force_lattices > 0 {
            accel = accel + face_gravity_force(x, origin, field_spacing, field_dims, force_base, u32(a));
        }
        var v = fma(accel, step_dt, e_faces.face_velocity[a]);
        if impulse {
            v = v + face_gravity_impulse(x, origin, field_spacing, field_dims, u32(a));
        }
        if p[a] == 0 {
            out.face_velocity[a] = max(v, 0.0);
        } else if p[a] == n[a] {
            out.face_velocity[a] = min(v, 0.0);
        } else {
            out.face_velocity[a] = v;
        }
    }
    return out;
}
