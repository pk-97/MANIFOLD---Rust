var<private> matter_pool_tiles: u32;

fn ts_pool_tiles() -> u32 { return matter_pool_tiles; }

fn ts_table_entries() -> u32 {
    let n = 4u * max(matter_pool_tiles, 1u);
    return 1u << (32u - countLeadingZeros(n - 1u));
}

fn ts_table_word(i: u32) -> u32 { return buf_tiles[i]; }

// node.matter_static_solid — BUFFER body. One thread per node of a listed
// world tile (`mss_node_of` over the list bound as `held`: every held tile
// when the static colliders changed, else the table build's fresh tiles). Node p sits at world
// p·cell_size. The element is the nearest static collider mesh (a body whose
// shape has tree == 1) packed for matter_bounds.wgsl: (n·(1 + row), φ) with φ
// the mesh's signed distance (band four cells, shell one, so an open sheet is
// a cell thick), n its outward unit normal and row its body, or (0, 0, 0, φ)
// with φ the band when no static mesh is near. The mesh search keeps a 64-entry stack per thread, which starves any
// kernel that carries it, so it runs here and the per-substep passes read
// the cache. `bodies`, `shapes` and the collider trees are gathered.

fn cb_node(i: u32) -> vec4<f32> {
    let n = buf_collider_nodes[i];
    return vec4<f32>(n.x, n.y, n.z, n.w);
}

fn cb_triangle(i: u32) -> vec4<f32> {
    let t = buf_collider_triangles[i];
    return vec4<f32>(t.x, t.y, t.z, t.w);
}

fn mss_node_of(t: u32) -> u32 {
    return buf_held[t >> 9u] * 512u + (t & 511u);
}

fn body(
    idx: u32,
    count: u32,
    cell_size: f32,
    pool_tiles: i32,
    body_count: i32,
    static_key: i32,
    built: i32,
) -> Element3 {
    matter_pool_tiles = u32(pool_tiles);
    ts_home(idx);
    let x = vec3<f32>(ts_cell_coord(idx)) * cell_size;
    let band = 4.0 * cell_size;
    var best = band;
    var best_body = -1;
    for (var b = 0; b < body_count; b = b + 1) {
        let bd = buf_bodies[u32(b)];
        let shape_index = i32(bd.accel_shape.w);
        if shape_index < 0 {
            continue;
        }
        let sh = buf_shapes[u32(shape_index)];
        if sh.tree != 1u {
            continue;
        }
        let phi = mt_phi(sh.tree_node, sh.tree_triangle, mt_local(x, bd.position_inv_mass.xyz, bd.rotation), cell_size);
        if phi < best {
            best = phi;
            best_body = b;
        }
    }
    var out: Element3;
    out.x = 0.0;
    out.y = 0.0;
    out.z = 0.0;
    out.w = best;
    if best_body < 0 {
        return out;
    }
    let bd = buf_bodies[u32(best_body)];
    let sh = buf_shapes[u32(bd.accel_shape.w)];
    let grad = mt_gradient(sh.tree_node, sh.tree_triangle, mt_local(x, bd.position_inv_mass.xyz, bd.rotation), cell_size, bd.rotation);
    let length_sq = dot(grad, grad);
    if !(length_sq > 0.0) {
        return out;
    }
    let packed = grad * inverseSqrt(length_sq) * (1.0 + f32(best_body));
    out.x = packed.x;
    out.y = packed.y;
    out.z = packed.z;
    return out;
}
