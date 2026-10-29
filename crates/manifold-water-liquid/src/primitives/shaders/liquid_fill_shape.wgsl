// A shaped Initial Fill's test, shared by every solver's fill. Declared
// through `wgsl_includes` after liquid_pose.wgsl, liquid_collider.wgsl and
// collider_bvh.wgsl; the including body gathers `regions` (LiquidBody),
// `shapes`, `atlas` and the collider trees, and defines liquid_atlas_half,
// cb_node and cb_triangle over them.

// Whether shaped fill region `r` (row r of `regions`) holds `x`: its exact
// mesh's distance (a tree shape, band four cells and shell one) or its
// lattice distance scaled to metres, at or below 0. A point the lattice does
// not cover is outside.
fn liquid_fill_in_shape(x: vec3<f32>, r: i32, cell: f32) -> bool {
    let bd = buf_regions[u32(r)];
    let shape_index = i32(bd.accel_shape.w);
    if shape_index < 0 {
        return false;
    }
    let sh = buf_shapes[u32(shape_index)];
    if sh.tree == 1u {
        let local = liquid_rotate(vec4<f32>(-bd.rotation.xyz, bd.rotation.w), x - bd.position_inv_mass.xyz);
        return cb_phi(sh.tree_node, sh.tree_triangle, local, 4.0 * cell, cell) <= 0.0;
    }
    let dims = vec3<u32>(sh.dims_x, sh.dims_y, sh.dims_z);
    let g = liquid_lattice_coord(x, bd.position_inv_mass.xyz, bd.rotation, sh.origin_spacing, sh.scale_min.xyz);
    return liquid_lattice_holds(g, dims) && liquid_lattice_distance(sh.atlas_offset, dims, g) * sh.scale_min.w <= 0.0;
}
