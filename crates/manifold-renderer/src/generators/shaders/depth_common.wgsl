// depth_common.wgsl — Shared depth-linearization helper for MANIFOLD's
// stored G-buffer depth (docs/GBUFFER_DESIGN.md §2 D4).
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
