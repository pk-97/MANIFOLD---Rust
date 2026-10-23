// view_normal_common.wgsl — a view-space normal from a depth buffer that
// stays on the surface at silhouettes. Shared by `node.ssao_gtao` (a
// `wgsl_includes`) and `node.temporal_accumulate` (prepended at pipeline
// creation). Ported from XeGTAO (Intel, MIT license; see
// THIRD_PARTY_NOTICES.md): `XeGTAO_CalculateEdges` and
// `XeGTAO_CalculateNormal`.
//
// A plain central difference straddles a silhouette, so the pixels next to
// an edge-on face get a normal half way to the background and draw a dark
// line. Instead each of the four neighbours gets an edge weight (near 0 when
// its depth leaves the local slope), and the normal is the weighted sum of
// the four corner normals (left-top, top-right, right-bottom, bottom-left),
// so a corner that reaches across an edge counts for almost nothing.
//
// Positions are view space: x right, y up, z forward depth. The normal
// faces the camera (negative z).

// Edge weights for the left, right, top and bottom neighbours from view
// depths: 1 on the surface's own slope, 0 across a depth edge.
fn view_normal_edges(center_z: f32, left_z: f32, right_z: f32, top_z: f32, bottom_z: f32) -> vec4<f32> {
    var edges = vec4<f32>(left_z, right_z, top_z, bottom_z) - center_z;
    let slope_lr = (edges.y - edges.x) * 0.5;
    let slope_tb = (edges.w - edges.z) * 0.5;
    let slope_adjusted = edges + vec4<f32>(slope_lr, -slope_lr, slope_tb, -slope_tb);
    edges = min(abs(edges), abs(slope_adjusted));
    return clamp(vec4<f32>(1.25) - edges / max(center_z * 0.011, 1e-6), vec4<f32>(0.0), vec4<f32>(1.0));
}

fn view_normal_dir(d: vec3<f32>) -> vec3<f32> {
    let len = length(d);
    return select(vec3<f32>(0.0), d / len, len > 1e-8);
}

// The camera-facing normal at `center` from its four neighbours' positions,
// each corner weighted by the edge weights of its two neighbours.
fn view_normal(edges: vec4<f32>, center: vec3<f32>, left: vec3<f32>, right: vec3<f32>, top: vec3<f32>, bottom: vec3<f32>) -> vec3<f32> {
    let accepted = clamp(
        vec4<f32>(edges.x * edges.z, edges.z * edges.y, edges.y * edges.w, edges.w * edges.x) + 0.01,
        vec4<f32>(0.0),
        vec4<f32>(1.0),
    );
    let l = view_normal_dir(left - center);
    let r = view_normal_dir(right - center);
    let t = view_normal_dir(top - center);
    let b = view_normal_dir(bottom - center);
    let n = accepted.x * cross(l, t) + accepted.y * cross(t, r) + accepted.z * cross(r, b) + accepted.w * cross(b, l);
    let len = length(n);
    return select(vec3<f32>(0.0, 0.0, -1.0), n / len, len > 1e-8);
}
