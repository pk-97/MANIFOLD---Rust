// depth_common.wgsl — Shared depth-linearization helper for MANIFOLD's
// stored G-buffer depth (docs/GBUFFER_DESIGN.md section 2 D4).
//
// Include via string concatenation at pipeline creation time, same
// convention as noise_common.wgsl:
//   let source = format!("{}\n{}", DEPTH_COMMON, MAIN_SHADER);
//
// `node.render_scene`'s `depth` output stores RAW [0,1] clip depth (D2) —
// NOT linearized — so every consumer must run it through `linearize_depth`
// rather than re-deriving the mapping inline (synthesis-drift is the
// forbidden move this file exists to prevent). The formula is the EXACT
// inverse of `perspective_rh`'s depth mapping
// (generators/mesh_pipeline.rs::perspective_rh):
//   range = far / (near - far)
//   raw   = range * (near / view_z - 1)      [forward mapping]
//   view_z = (range * near) / (raw + range)  [this file's inverse]
// `linearize_depth`'s Rust twin lives at
// `node_graph::camera::linearize_depth` — both MUST implement the exact
// same formula (I3's unit test checks them against the same oracle).
//
// No entry points — pure library, like noise_common.wgsl.

fn linearize_depth(raw: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return (range * near) / (raw + range);
}

// Exact inverse of linearize_depth (the forward `perspective_rh` depth
// mapping): raw = range * (near / view_z - 1). Consumers that average in
// linear eye depth (node.bilateral_blur value_space=ClipDepth) convert the
// result back through this, so the round trip is the shared convention and
// never a re-derived inline variant.
fn delinearize_depth(view_z: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return range * (near / view_z - 1.0);
}

// View-space position at integer texel `c` (clamped to the texture bounds)
// from a raw [0,1] depth value, pinhole perspective frame: the camera sits
// at the origin looking along +z, screen y is down (Metal viewport). This
// is THE shared view-reconstruction helper — node.ssao_gtao and
// node.normals_from_depth both build their normals from it, so the formula
// lives here exactly once (synthesis-drift is the forbidden move).
fn view_pos_from_depth(
    depth_tex: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(depth_tex, cc, 0).r;
    let view_z = linearize_depth(raw, near, far);
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    let ndc_x = uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - uv.y * 2.0;
    let view_x = ndc_x * tan_half_fov * aspect * view_z;
    let view_y = ndc_y * tan_half_fov * view_z;
    return vec3<f32>(view_x, view_y, view_z);
}
