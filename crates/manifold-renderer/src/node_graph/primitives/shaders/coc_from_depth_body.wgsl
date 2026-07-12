// node.coc_from_depth — fusable body (freeze §12), Pointwise + CoincidentTexel.
//
// Physically-based circle-of-confusion (thin-lens model) from scene depth +
// a Camera's fov/near/far/lens (docs/CINEMATIC_POST_DESIGN.md D1). Exact
// formula, no substitution:
//   f_mm    = SENSOR_H_MM / (2 * tan(fov_y / 2))
//   A_mm    = f_mm / f_stop
//   D_mm    = linearize_depth(raw_depth, near, far) * WORLD_TO_MM
//   S_mm    = focus_distance * WORLD_TO_MM
//   coc_mm  = A_mm * f_mm * abs(D_mm - S_mm) / (D_mm * max(S_mm - f_mm, 1.0))
//   coc_px  = clamp(coc_mm / SENSOR_H_MM * viewport_h, 0.0, max_radius)
//   out.r   = coc_px / max_radius   (normalized — node.variable_blur's `width`
//             convention: step_size = width_sample * max_radius + 1.0, so this
//             atom must emit a [0,1] FRACTION of ITS OWN max_radius, not raw
//             pixels; the preset wires this atom's max_radius equal to the
//             downstream variable_blur nodes' max_radius so the units agree.)
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
    let coc_mm = a_mm * f_mm * abs(d_mm - s_mm) / (d_mm * max(s_mm - f_mm, 1.0));
    let coc_px = clamp(coc_mm / SENSOR_H_MM * dims.y, 0.0, max_radius);
    let normalized = coc_px / max_radius;
    return vec4<f32>(normalized, normalized, normalized, 1.0);
}
