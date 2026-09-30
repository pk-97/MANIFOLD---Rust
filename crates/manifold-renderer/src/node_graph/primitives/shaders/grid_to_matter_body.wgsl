// node.grid_to_matter — fusable BUFFER body (GPU_MPM_SOLVER_DESIGN.md section
// 4.1 step 6). One thread per material point gathers its 27-node stencil from
// the resolved grid (`buf_grid`, Element2 = MatterGridNode):
//   v_pic = Σ w·v_i
//   v_p  ← β·(v_p + Σ w·(v_i − v_before_i)) + (1 − β)·v_pic     (β = Liveliness)
//   C_p  = (4/dx²)·Σ w·v_i ⊗ d_i,   x_p += dt·v_pic,   J ← J·(1 + dt·tr C)
// Positions advect with v_pic (AFLIP, Fei et al. 2021, as Blatny & Gaume 2025
// implement it). A point whose stencil leaves the lattice is removed (id 0).
// A point whose position is not finite is left as it is, for the stats to
// report. Element = MatterPoint.
//
// Colliders (D29): after the move, a point inside an enabled body (φ < 0 at
// the body's pose at the end of this substep, `bodies` from
// node.matter_move_bodies) steps along the lattice normal onto the surface,
// and the inward normal part of its velocity relative to the body is
// removed. Tangential motion is untouched and no relative speed is added.
// `bodies`, `shapes` and `atlas` are gathered; sampling is
// matter_collider.wgsl's.
fn g2m_finite3(v: vec3<f32>) -> bool {
    let e = vec3<u32>(bitcast<u32>(v.x), bitcast<u32>(v.y), bitcast<u32>(v.z)) & vec3<u32>(0x7f800000u);
    return all(e != vec3<u32>(0x7f800000u));
}

fn matter_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn body(
    idx: u32,
    count: u32,
    e_points: Element,
    lattice_min_x: f32,
    lattice_min_y: f32,
    lattice_min_z: f32,
    cell_size: f32,
    nodes_x: i32,
    nodes_y: i32,
    nodes_z: i32,
    step_dt: f32,
    liveliness: f32,
    cohesion: f32,
    active_count: i32,
    body_count: i32,
) -> Element {
    var p = e_points;
    if p.id == 0u || !g2m_finite3(p.position) {
        return p;
    }
    let origin = vec3<f32>(lattice_min_x, lattice_min_y, lattice_min_z);
    let inv_dx = 1.0 / cell_size;
    let nodes = vec3<i32>(nodes_x, nodes_y, nodes_z);
    let q = (p.position - origin) * inv_dx;
    let base = floor(q - vec3<f32>(0.5));
    let base_i = vec3<i32>(base);
    if any(base_i < vec3<i32>(0)) || any(base_i + vec3<i32>(2) > nodes - vec3<i32>(1)) {
        p.id = 0u;
        p.velocity = vec3<f32>(0.0);
        return p;
    }
    let f = q - base;
    let w0 = 0.5 * (vec3<f32>(1.5) - f) * (vec3<f32>(1.5) - f);
    let w1 = vec3<f32>(0.75) - (f - vec3<f32>(1.0)) * (f - vec3<f32>(1.0));
    let w2 = 0.5 * (f - vec3<f32>(0.5)) * (f - vec3<f32>(0.5));

    var v_pic = vec3<f32>(0.0);
    var flip_delta = vec3<f32>(0.0);
    var b0 = vec3<f32>(0.0);
    var b1 = vec3<f32>(0.0);
    var b2 = vec3<f32>(0.0);
    for (var a = 0; a < 3; a = a + 1) {
        var wx = w0.x;
        if a == 1 { wx = w1.x; } else if a == 2 { wx = w2.x; }
        for (var b = 0; b < 3; b = b + 1) {
            var wy = w0.y;
            if b == 1 { wy = w1.y; } else if b == 2 { wy = w2.y; }
            for (var c = 0; c < 3; c = c + 1) {
                var wz = w0.z;
                if c == 1 { wz = w1.z; } else if c == 2 { wz = w2.z; }
                let weight = wx * wy * wz;
                let d = (vec3<f32>(f32(a), f32(b), f32(c)) - f) * cell_size;
                let node = base_i + vec3<i32>(a, b, c);
                let g = buf_grid[u32((node.z * nodes.y + node.y) * nodes.x + node.x)];
                let vi = g.velocity_mass.xyz;
                v_pic = v_pic + weight * vi;
                flip_delta = flip_delta + weight * (vi - g.velocity_before.xyz);
                b0 = b0 + weight * vi.x * d;
                b1 = b1 + weight * vi.y * d;
                b2 = b2 + weight * vi.z * d;
            }
        }
    }
    let k = 4.0 * inv_dx * inv_dx;
    let c0 = k * b0;
    let c1 = k * b1;
    let c2 = k * b2;
    p.velocity = liveliness * (p.velocity + flip_delta) + (1.0 - liveliness) * v_pic;
    p.position = p.position + step_dt * v_pic;
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let centre = bd.position_inv_mass.xyz;
        let g = matter_lattice_coord(p.position, centre, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        if !matter_lattice_holds(g, dims) {
            continue;
        }
        let phi = matter_lattice_distance(sh.atlas_offset, dims, g);
        if phi >= 0.0 {
            continue;
        }
        let grad = matter_lattice_gradient(sh.atlas_offset, dims, g, sh.origin_spacing.w, bd.rotation, sh.scale_min.xyz);
        let length_sq = dot(grad, grad);
        if !(length_sq > 0.0) {
            continue;
        }
        let normal = grad * inverseSqrt(length_sq);
        // To φ = 0 along the normal, no farther than the surface can be.
        let max_scale = max(sh.scale_min.x, max(sh.scale_min.y, sh.scale_min.z));
        p.position = p.position + normal * min(-phi * inverseSqrt(length_sq), -phi * max_scale);
        let v_rel = p.velocity - matter_body_velocity(bd.linear_velocity.xyz, bd.angular_velocity.xyz, centre, p.position);
        let v_n = dot(v_rel, normal);
        if v_n < 0.0 {
            p.velocity = p.velocity - v_n * normal;
        }
    }
    // D3: tension-free water (Cohesion 0) stores no expansion; cohesive water
    // tears at twice its rest volume.
    let j_max = select(2.0, 1.0, cohesion <= 0.0);
    p.volume_ratio = min(p.volume_ratio * (1.0 + step_dt * (c0.x + c1.y + c2.z)), j_max);
    p.affine_x = vec4<f32>(c0, p.affine_x.w);
    p.affine_y = vec4<f32>(c1, p.affine_y.w);
    p.affine_z = vec4<f32>(c2, 0.0);

    let q_new = (p.position - origin) * inv_dx;
    let base_new = vec3<i32>(floor(q_new - vec3<f32>(0.5)));
    if any(base_new < vec3<i32>(0)) || any(base_new + vec3<i32>(2) > nodes - vec3<i32>(1)) {
        p.id = 0u;
        p.velocity = vec3<f32>(0.0);
    }
    return p;
}
