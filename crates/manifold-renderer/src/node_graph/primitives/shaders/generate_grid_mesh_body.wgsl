// node.grid_mesh — fusable BUFFER body (freeze §12, buffer domain), SOURCE.
// Emit a regular resolution_x × resolution_y grid of MeshVertex items in the
// XZ plane (Y=0), UV = grid index normalized to [0, 1]. Matches
// generate_grid_mesh.wgsl bit-for-bit (origin_x/origin_z are always 0.0 in the
// hand kernel too — not exposed as params, so they're not threaded here).
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the MeshVertex written to
// buf_vertices[idx]. `max_capacity` is an allocation-only param the shader
// ignores (DCE drops it). `dispatch_count` (= output capacity) is the wrapper
// guard; slots idx >= resolution_x * resolution_y are the inactive/padding
// vertices, cleared to the same dead vertex the hand kernel writes.
fn body(
    idx: u32,
    count: u32,
    max_capacity: i32,
    resolution_x: i32,
    resolution_y: i32,
    size_x: f32,
    size_y: f32,
) -> Element {
    let res_x = u32(resolution_x);
    let res_y = u32(resolution_y);

    if idx >= res_x * res_y {
        return Element(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let row = idx / res_x;
    let col = idx % res_x;
    let nx = f32(col) / f32(max(res_x - 1u, 1u));
    let nz = f32(row) / f32(max(res_y - 1u, 1u));

    let x = (nx - 0.5) * size_x;
    let z = (nz - 0.5) * size_y;

    return Element(vec3<f32>(x, 0.0, z), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(nx, nz));
}
