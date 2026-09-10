// node.normals_from_depth — fusable body (freeze section 12), 2-input
// Coincident with GATHERTEXEL on `depth` + `coverage` (docs/WATER_SIMULATION_DESIGN.md
// section 7). Reconstructs view-space positions with the shared
// depth_common.wgsl view_pos_from_depth helper (the same helper
// node.ssao_gtao builds its normals from — perspective water normals, not
// heightmap normals) and builds the surface normal from per-axis one-sided
// differences: for each axis, among the COVERED neighbours, the pair with
// the smallest depth discontinuity wins. Covered centre with no covered
// neighbour on an axis falls back to the single covered side; a pixel with
// no covered neighbours at all gets the toward-camera fallback (0,0,-1) —
// never a heightmap normal, never smoothing uncovered pixels into liquid.
// Output: view-space normal in RGB (toward-camera hemisphere = NEGATIVE z
// here: screen x right / screen y down makes cross(ddx, ddy) face the
// camera with z <= 0, see the flip below), coverage in A (0 empty,
// 1 occupied). Consumers using the depth_common view_pos frame (in-front =
// -z, toward-camera = +z) must negate z once — water_surface_pass.wgsl
// does.

// Covered test on the R8Unorm coverage texture (0 empty, 1 occupied).
fn nfd_covered(coverage_tex: texture_2d<f32>, c: vec2<i32>, dims_i: vec2<i32>) -> bool {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    return textureLoad(coverage_tex, cc, 0).r >= 0.5;
}

fn nfd_z(depth_tex: texture_2d<f32>, c: vec2<i32>, dims_i: vec2<i32>, near: f32, far: f32) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    return linearize_depth(textureLoad(depth_tex, cc, 0).r, near, far);
}

fn body(
    depth_tex: texture_2d<f32>,
    coverage_tex: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    fov_y: f32,
    near: f32,
    far: f32,
) -> vec4<f32> {
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);
    let tan_half_fov = tan(fov_y * 0.5);
    let aspect = dims.x / dims.y;

    if (!nfd_covered(coverage_tex, c, dims_i)) {
        // Empty stays empty (A=0); uncovered pixels are never smoothed
        // into liquid.
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    let p_c = view_pos_from_depth(depth_tex, c, dims_i, tan_half_fov, aspect, near, far);
    let z_c = p_c.z;

    // Per axis: prefer the covered neighbour pair with the smallest depth
    // discontinuity; a single covered side gives a one-sided difference.
    let xp = c + vec2<i32>(1, 0);
    let xm = c - vec2<i32>(1, 0);
    let yp = c + vec2<i32>(0, 1);
    let ym = c - vec2<i32>(0, 1);

    let xp_ok = nfd_covered(coverage_tex, xp, dims_i);
    let xm_ok = nfd_covered(coverage_tex, xm, dims_i);
    let yp_ok = nfd_covered(coverage_tex, yp, dims_i);
    let ym_ok = nfd_covered(coverage_tex, ym, dims_i);

    var ddx: vec3<f32>;
    if (xp_ok && xm_ok) {
        let z_xp = nfd_z(depth_tex, xp, dims_i, near, far);
        let z_xm = nfd_z(depth_tex, xm, dims_i, near, far);
        if (abs(z_xp - z_c) <= abs(z_c - z_xm)) {
            ddx = view_pos_from_depth(depth_tex, xp, dims_i, tan_half_fov, aspect, near, far) - p_c;
        } else {
            ddx = p_c - view_pos_from_depth(depth_tex, xm, dims_i, tan_half_fov, aspect, near, far);
        }
    } else if (xp_ok) {
        ddx = view_pos_from_depth(depth_tex, xp, dims_i, tan_half_fov, aspect, near, far) - p_c;
    } else if (xm_ok) {
        ddx = p_c - view_pos_from_depth(depth_tex, xm, dims_i, tan_half_fov, aspect, near, far);
    } else {
        ddx = vec3<f32>(0.0);
    }

    var ddy: vec3<f32>;
    if (yp_ok && ym_ok) {
        let z_yp = nfd_z(depth_tex, yp, dims_i, near, far);
        let z_ym = nfd_z(depth_tex, ym, dims_i, near, far);
        if (abs(z_yp - z_c) <= abs(z_c - z_ym)) {
            ddy = view_pos_from_depth(depth_tex, yp, dims_i, tan_half_fov, aspect, near, far) - p_c;
        } else {
            ddy = p_c - view_pos_from_depth(depth_tex, ym, dims_i, tan_half_fov, aspect, near, far);
        }
    } else if (yp_ok) {
        ddy = view_pos_from_depth(depth_tex, yp, dims_i, tan_half_fov, aspect, near, far) - p_c;
    } else if (ym_ok) {
        ddy = p_c - view_pos_from_depth(depth_tex, ym, dims_i, tan_half_fov, aspect, near, far);
    } else {
        ddy = vec3<f32>(0.0);
    }

    var normal = cross(ddx, ddy);
    let normal_len = length(normal);
    if (normal_len > 1e-8) {
        normal = normal / normal_len;
        // Orient into the toward-camera hemisphere (-z in this view
        // convention): screen x right, screen y down gives cross(ddx, ddy)
        // facing the camera for front surfaces, and this guard keeps edge
        // one-sided fallbacks from flipping away.
        if (normal.z > 0.0) {
            normal = -normal;
        }
    } else {
        // Isolated covered pixel (no covered neighbour on either axis):
        // toward-camera fallback, documented in the node contract.
        normal = vec3<f32>(0.0, 0.0, -1.0);
    }
    return vec4<f32>(normal, 1.0);
}
