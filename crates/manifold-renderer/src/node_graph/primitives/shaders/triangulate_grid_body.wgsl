// node.triangulate_grid — fusable BUFFER body (freeze §12, buffer domain),
// GATHER. Convert an NxM positions grid (Array<MeshVertex>) into a triangle-list
// of (N-1)*(M-1)*6 vertices with finite-difference normals. Matches
// triangulate_grid.wgsl bit-for-bit.
//
// ABI (buffer standalone codegen): `in` is a BUFFER GATHER input — the output
// vertex reads multiple grid cells (the quad corner + the 4 normal-difference
// neighbours), so the body indexes the input array global `buf_in` directly
// (no per-[idx] pre-read). The codegen synthesizes
//   struct Element { position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32> }
// from MeshVertex's Channels signature (in + out share it). `dispatch_count`
// (= dst capacity) is the wrapper guard; slots past the triangle count emit the
// degenerate padding vertex. src_cols/src_rows arrive as i32; the hand grid
// helpers referenced the params global, so here they take the dims as args and
// read `buf_in`.
fn tg_sample_pos(col: i32, row: i32, src_cols: i32, src_rows: i32) -> vec3<f32> {
    let cc = clamp(col, 0, src_cols - 1);
    let rr = clamp(row, 0, src_rows - 1);
    let idx = u32(rr) * u32(src_cols) + u32(cc);
    return buf_in[idx].position;
}

fn tg_sample_uv(col: i32, row: i32, src_cols: i32, src_rows: i32) -> vec2<f32> {
    let cc = clamp(col, 0, src_cols - 1);
    let rr = clamp(row, 0, src_rows - 1);
    let idx = u32(rr) * u32(src_cols) + u32(cc);
    return buf_in[idx].uv;
}

fn tg_compute_normal(col: i32, row: i32, src_cols: i32, src_rows: i32) -> vec3<f32> {
    let dx = tg_sample_pos(col + 1, row, src_cols, src_rows) - tg_sample_pos(col - 1, row, src_cols, src_rows);
    let dy = tg_sample_pos(col, row + 1, src_cols, src_rows) - tg_sample_pos(col, row - 1, src_cols, src_rows);
    let n = cross(dy, dx);
    let len = length(n);
    if len < 1e-8 {
        return vec3<f32>(0.0, 1.0, 0.0);
    }
    return n / len;
}

fn body(idx: u32, count: u32, src_cols: i32, src_rows: i32) -> Element {
    let quads_x = u32(src_cols) - 1u;
    let quads_y = u32(src_rows) - 1u;
    let total_verts = quads_x * quads_y * 6u;

    if idx >= total_verts {
        // Padding vertex past the triangle count.
        return Element(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let quad_idx = idx / 6u;
    let corner = idx % 6u;
    let qx = quad_idx % quads_x;
    let qy = quad_idx / quads_x;

    // Triangle layout: 0:(0,0) 1:(1,0) 2:(0,1) 3:(0,1) 4:(1,0) 5:(1,1).
    var dx: u32 = 0u;
    var dy: u32 = 0u;
    switch corner {
        case 0u: { dx = 0u; dy = 0u; }
        case 1u: { dx = 1u; dy = 0u; }
        case 2u: { dx = 0u; dy = 1u; }
        case 3u: { dx = 0u; dy = 1u; }
        case 4u: { dx = 1u; dy = 0u; }
        case 5u: { dx = 1u; dy = 1u; }
        default: {}
    }

    let col = i32(qx + dx);
    let row = i32(qy + dy);
    let pos = tg_sample_pos(col, row, src_cols, src_rows);
    let normal = tg_compute_normal(col, row, src_cols, src_rows);
    let uv = tg_sample_uv(col, row, src_cols, src_rows);

    return Element(pos, normal, uv);
}
