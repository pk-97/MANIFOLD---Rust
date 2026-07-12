// node.project_3d — fusable BUFFER body (freeze §12, buffer domain), COINCIDENT
// type-changing. Project a 3D mesh vertex to a 2D curve point, orthographic
// (out.xy = pos.xy * proj_scale) or perspective (s = proj_dist / (proj_dist +
// z)). Origin-centered output (render_lines applies the aspect + screen-shift).
// Matches project_3d.wgsl. LEGACY MODE MATH IS UNTOUCHED — bit-identical to
// before this file grew a camera branch (docs/CAMERA_AND_LENS_DESIGN.md D3,
// invariant I3).
//
// ABI (buffer standalone codegen): the input `in` (MeshVertex) is coincident, so
// the wrapper pre-reads `e_in = buf_in[idx]` and passes it. The codegen
// synthesizes from the Channels signatures:
//   struct Element  { position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32> }  // MeshVertex
//   struct Element2 { x: f32, y: f32 }                                          // CurvePoint
// `dispatch_count` (= output capacity) is the guard; `active_count` is a DERIVED
// uniform (f32, cast to u32 — exact for these small vertex counts). Slots in
// [active_count, capacity) collapse to origin. `mode` is the Enum param (u32).
//
// Camera branch (D3, optional `camera: Camera` port, port-shadows-param;
// Amendment 2026-07-12 pins the exact derived-field set below after the
// original clip-matrix-row proposal was found to break I3's byte-packed
// parity test — see docs/CAMERA_AND_LENS_DESIGN.md). When `use_camera`
// (derived u32 flag, 1 = wired) is set, every point projects through the
// camera's own orthonormal basis instead of the legacy mode math:
//
//   rel    = pos - cam_pos
//   view_z = dot(rel, cam_fwd)                 // forward distance, +ve = in front
//   ndc.x  = proj_f * dot(rel, cam_right) / view_z
//   ndc.y  = proj_f * dot(rel, cam_up)    / view_z
//
// `proj_f` (= 1/tan(fov_y/2) at aspect 1, resolved CPU-side from the wired
// Camera's mode) and `cam_near` (the wired camera's near-plane cull
// threshold) arrive as derived uniforms — this is algebraically the same
// `cam.proj(1.0) * cam.view * vec4(pos,1)` clip computation D3 specifies,
// just carried as camera-basis + scale rather than a pre-multiplied matrix,
// so no per-node projection formula is reinvented here (D1). `view_z <=
// cam_near` culls behind-camera points by collapsing to the origin — the
// same convention this atom already uses for inactive slots.
//
// CAMERA_Y_SIGN is the `S` from D3's mapping `out.xy = vec2(ndc.x*0.5, S *
// ndc.y*0.5)`. draw_lines' `curve_to_screen` (render_lines.wgsl:63) computes
// `screen = (p.x/aspect + 0.5, p.y + 0.5)` with NO y-flip of its own —
// Metal's rasterizer performs the NDC-to-framebuffer y-flip when it maps the
// vertex shader's `@builtin(position)` clip output to screen space. That
// means `Camera::project_to_pixel`'s y-down pixel formula and draw_lines'
// pixel path already agree WITHOUT an extra sign flip here, so S = +1 is the
// value the P1 `camera_conformance` gate confirms (frozen here as a named
// constant per the gate result — do not flip without rerunning the gate).
const CAMERA_Y_SIGN: f32 = 1.0;

fn body(
    idx: u32,
    count: u32,
    e_in: Element,
    mode: u32,
    proj_scale: f32,
    proj_dist: f32,
    active_count: f32,
    cam_right: vec3<f32>,
    cam_up: vec3<f32>,
    cam_fwd: vec3<f32>,
    cam_pos: vec3<f32>,
    proj_f: f32,
    cam_near: f32,
    use_camera: u32,
) -> Element2 {
    if idx >= u32(active_count) {
        return Element2(0.0, 0.0);
    }

    let p = e_in.position;

    if use_camera != 0u {
        let rel = p - cam_pos;
        let view_z = dot(rel, cam_fwd);
        if view_z <= cam_near {
            return Element2(0.0, 0.0);
        }
        let ndc_x = proj_f * dot(rel, cam_right) / view_z;
        let ndc_y = proj_f * dot(rel, cam_up) / view_z;
        return Element2(ndc_x * 0.5, CAMERA_Y_SIGN * ndc_y * 0.5);
    }

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
