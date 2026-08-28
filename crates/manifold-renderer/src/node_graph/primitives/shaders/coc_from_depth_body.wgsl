// node.coc_from_depth — fusable body (freeze section 12), Pointwise + CoincidentTexel.
//
// Physically-based circle-of-confusion (thin-lens model) from scene depth +
// a Camera's fov/near/far/lens (docs/CINEMATIC_POST_DESIGN.md D1). Exact
// formula, no substitution:
//   f_mm    = SENSOR_H_MM / (2 * tan(fov_y / 2))
//   A_mm    = f_mm / f_stop
//   D_mm    = linearize_depth(raw_depth, near, far) * WORLD_TO_MM
//   S_mm    = focus_distance * WORLD_TO_MM
//   signed  = D_mm - S_mm
//   coc_mm  = A_mm * f_mm * signed / (D_mm * max(S_mm - f_mm, 1.0))
//   coc_px  = clamp(|coc_mm| / SENSOR_H_MM * viewport_h, 0.0, max_radius)
//   out.r   = coc_px / max_radius   (MAGNITUDE — unchanged for every existing
//             reader: node.variable_blur and node.bokeh_gather read `width.r`)
//   out.g   = signed < 0 ? 1.0 : 0.0   (sign flag: 1.0 = nearer than focus,
//             0.0 = far-or-in-focus; docs/BOKEH_LAYERED_DOF_DESIGN.md D1)
//   out.b   = out.r   (copy of magnitude)
//   out.a   = 1.0
//
// `depth` is CoincidentTexel (own-texel integer textureLoad, no sampler) —
// render_scene's `depth` output stores RAW [0,1] clip depth, matching every
// other depth consumer's contract. `linearize_depth` comes from the SHARED
// depth_common.wgsl header (wgsl_includes) — never re-derived inline, per the
// synthesis-drift rule documented on `node_graph::camera::linearize_depth`.
//
// `camera` is a Camera-typed CPU-struct input consumed ENTIRELY via the five
// DERIVED_UNIFORMS below (fov_y/near/far are projection facts; focus_distance/
// f_stop are the Camera's lens block, written upstream by node.camera_lens —
// "one lens, every consumer reads it", docs/CAMERA_AND_LENS_DESIGN.md D4) —
// it never becomes a GPU binding, which is what lets this atom fuse with a
// pointwise neighbour instead of being a permanent boundary (P0/D7).
//
// PARAMS: [max_radius]. DERIVED_UNIFORMS: [fov_y, near, far, focus_distance,
// f_stop]. Matches coc_from_depth.wgsl (the hand parity oracle).
const SENSOR_H_MM: f32 = 24.0;
const WORLD_TO_MM: f32 = 1000.0;

fn body(
    c_depth: vec4<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    max_radius: f32,
    fov_y: f32,
    near: f32,
    far: f32,
    focus_distance: f32,
    f_stop: f32,
) -> vec4<f32> {
    let f_mm = SENSOR_H_MM / (2.0 * tan(fov_y * 0.5));
    let a_mm = f_mm / f_stop;
    let d_mm = linearize_depth(c_depth.r, near, far) * WORLD_TO_MM;
    let s_mm = focus_distance * WORLD_TO_MM;
    let signed_delta = d_mm - s_mm;
    let coc_mm = a_mm * f_mm * signed_delta / (d_mm * max(s_mm - f_mm, 1.0));
    let coc_px = clamp(abs(coc_mm) / SENSOR_H_MM * dims.y, 0.0, max_radius);
    // focus_distance <= 0 is the LensParams hyperfocal/neutral contract —
    // exactly 0 CoC. Without this, S_mm = 0 makes the denominator 1 and the
    // formula degenerates to f_mm^2/f_stop: MAX blur at every aperture
    // (Peter's 2026-08-27 fully-soft frame after dragging Focus to 0).
    let coc_q = select(coc_px, 0.0, focus_distance <= 0.0);
    let normalized = coc_q / max_radius;
    // Sign flag: 1.0 = nearer than focus, 0.0 = far-or-in-focus.
    let near_flag = select(0.0, 1.0, (signed_delta < 0.0) && (focus_distance > 0.0));
    return vec4<f32>(normalized, near_flag, normalized, 1.0);
}
