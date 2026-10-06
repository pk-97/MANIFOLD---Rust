// The arithmetic lives in particle_volume_common.wgsl, shared with the
// cooperative pass-1 kernel. Preview-only border replacement is not part of
// the production mesher.

fn body(
    idx: u32,
    count: u32,
    center_x: f32,
    center_y: f32,
    center_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    cell_size: f32,
    resolution_scale: i32,
    bins_x: i32,
    bins_y: i32,
    bins_z: i32,
    band_extra: f32,
    brick_pass: u32,
    interior_len: u32,
) -> f32 {
    let f = pv_frame(
        vec3<f32>(center_x, center_y, center_z),
        vec3<f32>(size_x, size_y, size_z),
        vec3<f32>(nodes_x, nodes_y, nodes_z),
        resolution_scale,
        band_extra,
        vec2<f32>(buf_bounds[0], buf_bounds[1]),
    );
    let bins = vec3<i32>(bins_x, bins_y, bins_z);
    if idx >= f.nodes.x * f.nodes.y * f.nodes.z {
        return f.band;
    }
    if any(bins < vec3<i32>(1)) && brick_pass != 2u {
        return f.band;
    }
    let ijk = pv_ijk(idx, f.nodes);
    let p = pv_position(ijk, f);

    var phi = f.band;
    if brick_pass != 2u {
        let w = pv_window(p, f, cell_size, bins);
        for (var z = w.first_bin.z; z <= w.last_bin.z; z = z + 1) {
            for (var y = w.first_bin.y; y <= w.last_bin.y; y = y + 1) {
                for (var x = w.first_bin.x; x <= w.last_bin.x; x = x + 1) {
                    let range = buf_cell_ranges[u32(x + bins.x * (y + bins.y * z))];
                    for (var k = range.start; k < range.start + range.count; k = k + 1u) {
                        let blob = buf_blobs[k];
                        if pv_box_rejects(blob.center_radius.w, ijk, pv_blob_box(blob.center_radius, f)) {
                            continue;
                        }
                        phi = min(phi, pv_blob_term(p, blob.center_radius, blob.shape_diag.xyz, blob.shape_off.xyz));
                    }
                }
            }
        }
    }
    return pv_finish(phi, p, f, interior_len);
}

fn liquid_brick_index(invocation: u32) -> u32 {
    let dims = (max(vec3<u32>(vec3<f32>(params.nodes_x, params.nodes_y, params.nodes_z)), vec3<u32>(2u)) - vec3<u32>(1u)) * u32(clamp(params.resolution_scale, 1, 8)) + vec3<u32>(1u);
    return liquid_brick_select(invocation, dims, params.brick_pass);
}
