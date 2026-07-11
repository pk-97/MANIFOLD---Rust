// node.extrude_curve — fusable BUFFER body (freeze §12, buffer domain),
// GATHER. Extrude a P-point outline curve (Array<CurvePoint>, x/y in curve
// space) along +Z into a (steps+1)×cols positions+uv grid (MESH_DEFORM_AND_
// CURVE_GEOMETRY_DESIGN.md D5 — normals left zero; wire node.make_triangles
// downstream). `cols = P` normally, `P+1` when `close` duplicates the first
// outline point as the last column (a closed loop). No end caps in v1
// (Deferred #3 — the extruded solid is open at both ends). Matches
// extrude_curve.wgsl bit-for-bit.
//
// ABI: `outline` is a BUFFER GATHER input — the body indexes `buf_outline`
// directly at `col % outline_len` (the closed-loop wrap when `close` adds a
// duplicate column). `outline_len` is the derived element count.
fn body(idx: u32, count: u32, depth: f32, steps: i32, close: u32, outline_len: u32) -> Element2 {
    let p_len = max(i32(outline_len), 1);
    let is_closed = close != 0u;
    var cols = p_len;
    if is_closed {
        cols = p_len + 1;
    }
    let rows = steps + 1;
    let total = u32(cols * rows);
    if idx >= total {
        return Element2(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let col = i32(idx) % cols;
    let row = i32(idx) / cols;
    let outline_col = col % p_len;
    let pt = buf_outline[u32(outline_col)];

    let row_denom = max(f32(steps), 1.0);
    let col_denom = max(f32(cols - 1), 1.0);

    let pos = vec3<f32>(pt.x, pt.y, depth * f32(row) / row_denom);
    let uv = vec2<f32>(f32(col) / col_denom, f32(row) / row_denom);

    return Element2(pos, vec3<f32>(0.0, 0.0, 0.0), uv);
}
