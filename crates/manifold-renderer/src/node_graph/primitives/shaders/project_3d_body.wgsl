// node.project_3d — fusable BUFFER body (freeze §12, buffer domain), COINCIDENT
// type-changing. Project a 3D mesh vertex to a 2D curve point, orthographic
// (out.xy = pos.xy * proj_scale) or perspective (s = proj_dist / (proj_dist +
// z)). Origin-centered output (render_lines applies the aspect + screen-shift).
// Matches project_3d.wgsl.
//
// ABI (buffer standalone codegen): the input `in` (MeshVertex) is coincident, so
// the wrapper pre-reads `e_in = buf_in[idx]` and passes it. The codegen
// synthesizes from the Channels signatures:
//   struct Element  { position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32> }  // MeshVertex
//   struct Element2 { x: f32, y: f32 }                                          // CurvePoint
// `dispatch_count` (= output capacity) is the guard; `active_count` is a DERIVED
// uniform (f32, cast to u32 — exact for these small vertex counts). Slots in
// [active_count, capacity) collapse to origin. `mode` is the Enum param (u32).
fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    mode: u32,
    proj_scale: f32,
    proj_dist: f32,
    active_count: f32,
) -> Element2 {
    if idx >= u32(active_count) {
        return Element2(0.0, 0.0);
    }

    let p = e_in.position;
    var x: f32;
    var y: f32;
    if mode == 1u {
        // Perspective: s = proj_dist / (proj_dist + z)
        let dz = proj_dist + p.z;
        let s = proj_dist / max(dz, 0.001);
        x = p.x * s * proj_scale;
        y = p.y * s * proj_scale;
    } else {
        // Orthographic (matches WireframeZoo)
        x = p.x * proj_scale;
        y = p.y * proj_scale;
    }
    return Element2(x, y);
}
