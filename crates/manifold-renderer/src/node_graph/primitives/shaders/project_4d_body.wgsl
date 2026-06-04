// node.project_4d — fusable BUFFER body (freeze §12, buffer domain), COINCIDENT
// type-changing. Project a 4D vertex to a 2D curve point via two-stage
// perspective (4D -> 3D collapse, then 3D -> 2D). Matches generator_math::
// project_4d bit-for-bit. Output is origin-centered pre-aspect curve space
// (render_lines applies the aspect + screen-shift).
//
// ABI (buffer standalone codegen): the input `in` (Vec4Vertex) is coincident, so
// the wrapper pre-reads `e_in = buf_in[idx]` and passes it; the body returns the
// output element written to buf_out[idx]. The codegen synthesizes the element
// structs from the Channels signatures:
//   struct Element  { x: f32, y: f32, z: f32, w: f32 }   // Vec4Vertex (input)
//   struct Element2 { x: f32, y: f32 }                   // CurvePoint (output)
// `dispatch_count` (= the OUTPUT capacity) is the guard; `active_count` is a
// DERIVED uniform (declared `derived_uniforms`, packed by run() = live vertex
// count) carried as f32 and cast to u32 — exact for these small vertex counts.
// Slots in [active_count, capacity) collapse to origin (the inactive sentinel
// render_lines drops). Matches project_4d.wgsl.
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    proj_scale: f32,
    proj_dist: f32,
    active_count: f32,
) -> Element2 {
    if idx >= u32(active_count) {
        // Inactive slot -> origin (zero-length degenerate point).
        return Element2(0.0, 0.0);
    }

    let dw = proj_dist - e_in.w;
    let f = proj_dist / select(dw, 0.001, abs(dw) < 0.001);
    let p3 = vec3<f32>(e_in.x, e_in.y, e_in.z) * f;

    let dz = proj_dist + p3.z;
    let s = proj_dist / select(dz, 0.001, abs(dz) < 0.001);

    let px = p3.x * s * proj_scale;
    let py = p3.y * s * proj_scale;
    return Element2(px, py);
}
