var<private> matter_pool_tiles: u32;

fn ts_pool_tiles() -> u32 { return matter_pool_tiles; }


fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.grid_to_matter — fusable BUFFER body. One thread per material point gathers its 27-node stencil from
// the resolved grid (`buf_grid`, Element2 = MatterGridNode):
//   v_p = Σ w·v_i,   C_p = (4/dx²)·Σ w·v_i ⊗ d_i,   x_p += dt·v_p
// Pure APIC: sand needs no FLIP blend, and APIC keeps its angular momentum.
// Then the material stage (matter_material.wgsl): F advects by
// C over dt, the return map keeps it inside the yield cone, and the record
// takes the new F, J = det F, its plastic state (volume correction in
// affine_x.w, plastic strain in f_x.w) and the Kirchhoff stress P2G spreads.
// The material is the point's row of `materials`, the slot in affine_z.w (kept),
// held to the stiffness the substep has room for (`matter_capped`). Node p sits
// at world p·h, pooled by world tile (READ_SIDE).
// A point whose own cell (the one holding it) has no stored tile is removed
// (id 0, velocity 0, volume ratio 0, which node.matter_stats counts as a short
// pool). After the move, a point beyond an Open face of the bounds box or below
// kill_height is removed too (id 0, velocity 0, volume ratio kept).
// A point whose position is not finite is left as it is, for the stats to
// report. Element = MatterPoint.
//
// Colliders (D29): after the move, a point inside an enabled body (φ < 0 at
// the body's pose at the end of this substep, `bodies` from
// node.matter_move_bodies) steps along the lattice normal onto the surface,
// and the inward normal part of its velocity relative to the body is
// removed. Tangential motion is untouched and no relative speed is added.
// `bodies`, `shapes` and `atlas` are gathered; sampling is
// liquid_collider.wgsl's. Static meshes (tree shapes) are read from
// node.matter_static_solid's cache (`static_solid`, gathered): φ trilinear
// over the 8 nodes round the moved point, its normal from the same corners
// and its body from the deepest; they are prescribed, so they take no
// reaction.
//
// Reaction (D30): the momentum a point loses there, m·v_n·n with
// m = V0 times its row's density, goes to a dynamic body (inv_mass > 0) as the same four
// terms node.matter_body_reaction adds per node, into the same 16 words of
// `reaction_out` (atomic; `reaction` aliases it and is never read). Only
// when dynamic_count > 0, which run() clears without a covering slot.
fn g2m_finite3(v: vec3<f32>) -> bool {
    let e = vec3<u32>(bitcast<u32>(v.x), bitcast<u32>(v.y), bitcast<u32>(v.z)) & vec3<u32>(0x7f800000u);
    return all(e != vec3<u32>(0x7f800000u));
}

fn liquid_atlas_half(index: u32) -> f32 {
    let pair = unpack2x16float(buf_atlas[index / 2u]);
    return select(pair.x, pair.y, (index & 1u) == 1u);
}

fn g2m_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

// floor(x + u) with the carry in integers: an f32 sum x + u would round first
// and bias every word upward by about |x|·2^-24.
fn g2m_encode(x: f32, key: u32, slot: u32) -> i32 {
    let whole = floor(x);
    let fraction = u32((x - whole) * 16777216.0);
    let carry = (fraction + (g2m_hash(key ^ slot) >> 8u)) >> 24u;
    return i32(whole) + i32(carry);
}

fn g2m_reaction_add(base: u32, value: vec3<f32>, key: u32) {
    for (var c = 0u; c < 3u; c = c + 1u) {
        let word = base + c;
        let encoded = g2m_encode(value[c], key, word);
        if encoded != 0 {
            atomicAdd(&buf_reaction_out[word], encoded);
        }
    }
}

fn body(
    idx: u32,
    count: u32,
    e_points: Element,
    cell_size: f32,
    step_dt: f32,
    active_count: i32,
    body_count: i32,
    momentum_unit: f32,
    tick_index: i32,
    substep_in_tick: i32,
    substeps_per_tick: i32,
    dynamic_count: i32,
    pool_tiles: i32,
    open_faces: i32,
    bounds_lo_x: f32,
    bounds_lo_y: f32,
    bounds_lo_z: f32,
    bounds_hi_x: f32,
    bounds_hi_y: f32,
    bounds_hi_z: f32,
    kill_height: f32,
    wave_reach: f32,
    speed_estimate: f32,
) -> Element {
    matter_pool_tiles = u32(pool_tiles);
    var p = e_points;
    if p.id == 0u || !g2m_finite3(p.position) {
        return p;
    }
    let inv_dx = 1.0 / cell_size;
    let own = ts_world_cell(p.position, vec3<f32>(-0.5 * cell_size), inv_dx);
    ts_home(ts_cell_at(own));
    if !ts_cell_stored(own) {
        p.id = 0u;
        p.velocity = vec3<f32>(0.0);
        p.volume_ratio = 0.0;
        return p;
    }
    let q = p.position * inv_dx;
    let base = floor(q - vec3<f32>(0.5));
    let base_i = vec3<i32>(base);
    let f = q - base;
    let w0 = 0.5 * (vec3<f32>(1.5) - f) * (vec3<f32>(1.5) - f);
    let w1 = vec3<f32>(0.75) - (f - vec3<f32>(1.0)) * (f - vec3<f32>(1.0));
    let w2 = 0.5 * (f - vec3<f32>(0.5)) * (f - vec3<f32>(0.5));

    var v_pic = vec3<f32>(0.0);
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
                let g = buf_grid[ts_cell_at(node)];
                let vi = g.velocity_mass.xyz;
                v_pic = v_pic + weight * vi;
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
    p.velocity = v_pic;
    p.position = p.position + step_dt * v_pic;
    let row_at = matter_slot(p.affine_z.w) * 16u;
    let row = MatterRow(
        vec4<f32>(buf_materials[row_at], buf_materials[row_at + 1u], buf_materials[row_at + 2u], buf_materials[row_at + 3u]),
        vec4<f32>(buf_materials[row_at + 4u], buf_materials[row_at + 5u], buf_materials[row_at + 6u], buf_materials[row_at + 7u]),
        vec4<f32>(buf_materials[row_at + 8u], buf_materials[row_at + 9u], buf_materials[row_at + 10u], buf_materials[row_at + 11u]),
        vec4<f32>(buf_materials[row_at + 12u], buf_materials[row_at + 13u], buf_materials[row_at + 14u], buf_materials[row_at + 15u]),
    );
    let mass = p.affine_y.w * row.c.y;
    let counts = 16777216.0 / momentum_unit;
    let weight = f32(substep_in_tick) / f32(max(substeps_per_tick, 1));
    // Salted apart from node.matter_body_reaction's node keys, which share
    // the words.
    let key = g2m_hash(idx ^ 0x9e3779b9u ^ g2m_hash(u32(tick_index) * 4096u + u32(substep_in_tick)));
    let sq = p.position * inv_dx;
    let s0 = floor(sq);
    let sf = sq - s0;
    var s_phi = 0.0;
    var s_grad = vec3<f32>(0.0);
    var s_row = -1;
    var s_deep = 0.0;
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let o = vec3<u32>(corner & 1u, (corner >> 1u) & 1u, corner >> 2u);
        let at = ts_cell_at(vec3<i32>(s0) + vec3<i32>(o));
        // A corner off the stored tiles reads as clear of every mesh.
        var e = vec4<f32>(0.0, 0.0, 0.0, 4.0 * cell_size);
        if at < ts_canon() {
            let c = buf_static_solid[at];
            e = vec4<f32>(c.x, c.y, c.z, c.w);
        }
        let side = vec3<f32>(o);
        let wv = mix(vec3<f32>(1.0) - sf, sf, side);
        let dw = 2.0 * side - vec3<f32>(1.0);
        s_phi = s_phi + wv.x * wv.y * wv.z * e.w;
        s_grad = s_grad + e.w * vec3<f32>(dw.x * wv.y * wv.z, wv.x * dw.y * wv.z, wv.x * wv.y * dw.z);
        let row = matter_static_row(e);
        if row >= 0 && e.w < s_deep {
            s_deep = e.w;
            s_row = row;
        }
    }
    let s_len_sq = dot(s_grad, s_grad) * inv_dx * inv_dx;
    if s_phi < 0.0 && s_row >= 0 && s_row < body_count && s_len_sq > 0.0 {
        let normal = s_grad * inv_dx * inverseSqrt(s_len_sq);
        p.position = p.position + normal * min(-s_phi * inverseSqrt(s_len_sq), -s_phi);
        let sb = buf_bodies[u32(s_row)];
        let v_rel = p.velocity - liquid_body_velocity(sb.linear_velocity.xyz, sb.angular_velocity.xyz, sb.position_inv_mass.xyz, p.position);
        let v_n = dot(v_rel, normal);
        if v_n < 0.0 {
            p.velocity = p.velocity - v_n * normal;
        }
    }
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
        let centre = bd.position_inv_mass.xyz;
        // A static mesh was read from the cache above.
        if sh.tree == 1u {
            continue;
        }
        let g = liquid_lattice_coord(p.position, centre, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
        if !liquid_lattice_holds(g, dims) {
            continue;
        }
        let phi = liquid_lattice_distance(sh.atlas_offset, dims, g);
        if phi >= 0.0 {
            continue;
        }
        let grad = liquid_lattice_gradient(sh.atlas_offset, dims, g, sh.origin_spacing.w, bd.rotation, sh.scale_min.xyz);
        // To φ = 0 along the normal, no farther than the surface can be.
        let max_scale = max(sh.scale_min.x, max(sh.scale_min.y, sh.scale_min.z));
        let length_sq = dot(grad, grad);
        if !(length_sq > 0.0) {
            continue;
        }
        let normal = grad * inverseSqrt(length_sq);
        p.position = p.position + normal * min(-phi * inverseSqrt(length_sq), -phi * max_scale);
        let v_rel = p.velocity - liquid_body_velocity(bd.linear_velocity.xyz, bd.angular_velocity.xyz, centre, p.position);
        let v_n = dot(v_rel, normal);
        if v_n < 0.0 {
            p.velocity = p.velocity - v_n * normal;
            let inv_mass = bd.position_inv_mass.w;
            if dynamic_count > 0 && inv_mass > 0.0 {
                let impulse = mass * v_n * normal;
                let dv = inv_mass * impulse * counts;
                let dl = inv_mass * cross(p.position - centre, impulse) / cell_size * counts;
                let words = u32(b) * 16u;
                g2m_reaction_add(words, dv, key);
                g2m_reaction_add(words + 3u, weight * dv, key);
                g2m_reaction_add(words + 6u, dl, key);
                g2m_reaction_add(words + 9u, weight * dl, key);
            }
        }
    }
    // Records hold rows; a WGSL matrix is built from columns.
    let up = matter_update(
        transpose(mat3x3<f32>(p.f_x.xyz, p.f_y.xyz, p.f_z.xyz)),
        transpose(mat3x3<f32>(c0, c1, c2)),
        step_dt,
        MatterPlastic(p.affine_x.w, p.f_x.w),
        matter_capped(row, cell_size, step_dt, wave_reach, speed_estimate, p.f_y.w, p.f_z.w),
    );
    let f_rows = transpose(up.f);
    p.volume_ratio = up.j;
    p.affine_x = vec4<f32>(c0, up.plastic.volume_correction);
    p.affine_y = vec4<f32>(c1, p.affine_y.w);
    p.affine_z = vec4<f32>(c2, p.affine_z.w);
    p.f_x = vec4<f32>(f_rows[0], up.plastic.strain);
    // w: the point's wetness and its share under water, kept for
    // node.matter_state, which sets them once a frame.
    p.f_y = vec4<f32>(f_rows[1], p.f_y.w);
    p.f_z = vec4<f32>(f_rows[2], p.f_z.w);
    p.stress_diag = vec4<f32>(up.tau[0][0], up.tau[1][1], up.tau[2][2], 0.0);
    p.stress_off = vec4<f32>(up.tau[1][0], up.tau[2][0], up.tau[2][1], 0.0);

    let lo = vec3<f32>(bounds_lo_x, bounds_lo_y, bounds_lo_z);
    let hi = vec3<f32>(bounds_hi_x, bounds_hi_y, bounds_hi_z);
    var removed = p.position.y < kill_height;
    for (var a = 0; a < 3; a = a + 1) {
        if ((open_faces >> u32(2 * a)) & 1) != 0 && p.position[a] < lo[a] {
            removed = true;
        }
        if ((open_faces >> u32(2 * a + 1)) & 1) != 0 && p.position[a] > hi[a] {
            removed = true;
        }
    }
    if removed {
        p.id = 0u;
        p.velocity = vec3<f32>(0.0);
    }
    return p;
}
