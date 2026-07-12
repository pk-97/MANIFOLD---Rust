// node.ssao_from_depth — fusable body (freeze §12), Pointwise + GatherTexel.
//
// Screen-space ambient occlusion from reconstructed view-space normals (no
// normal G-buffer in v1, docs/CINEMATIC_POST_DESIGN.md D3). Exact algorithm,
// no substitution:
//   1. Reconstruct view-space position per texel from raw depth
//      (linearize_depth + inverse-perspective xy using fov_y/near/far from
//      the Camera; aspect recovered from `dims`).
//   2. normal = normalize(cross(P(x+1)-P(x-1), P(y+1)-P(y-1))) from explicit
//      +/-1-texel INTEGER reads (GatherTexel — no derivative intrinsics;
//      compute has no fragment derivatives, and texel-exact reads are what
//      the CPU reference replicates exactly).
//   3. N=16 golden-angle spiral (docs/CINEMATIC_POST_DESIGN.md D2:
//      r_i = sqrt((i+0.5)/N), theta_i = i*2.399963), lifted onto the
//      normal's hemisphere via Malley's method (z_i = sqrt(1 - r_i^2))
//      around a tangent basis built from the reconstructed normal, rotated
//      per-pixel by D2's committed hash.
//   4. Each sample: sample_pos = P_center + kernel_vec*radius; reproject to
//      a texel (nearest, not bilinear — depth is non-linear across
//      silhouette edges); occlusion += 1 when the ACTUAL scene depth there
//      is nearer than the sample point (minus `bias`) AND within `radius`
//      of the center depth (halo guard).
//   5. out.r = 1 - intensity*occlusion/N (broadcast to RGB, alpha 1).
//
// `depth` is GatherTexel (integer textureLoad, manual ClampToEdge — no
// sampler) for BOTH the +/-1-texel normal reads and the N=16 reprojected
// occluder reads (nearest-neighbour, not bilinear — depth is non-linear
// across silhouette edges, and point sampling is what the CPU reference
// replicates exactly).
//
// `camera` reads fov_y/near/far entirely via the three DERIVED_UNIFORMS
// below — never a GPU binding, letting this Pointwise atom fuse with a
// neighbour instead of being a permanent boundary (P0/D7).
//
// PARAMS: [radius, intensity, bias]. DERIVED_UNIFORMS: [fov_y, near, far].
// Matches ssao_from_depth.wgsl (the hand parity oracle) — kept independent
// (not sharing source) so the gpu_tests parity check is a real cross-check.

const SSAO_N: u32 = 16u;
const SSAO_GOLDEN_ANGLE: f32 = 2.399963;

// D2's committed per-pixel rotation hash (docs/CINEMATIC_POST_DESIGN.md D2) —
// same hash base as film_grain_body.wgsl's white_noise, scaled to radians so
// it can be added directly to theta_i.
fn ssao_hash_angle(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453) * 6.283185307;
}

// Reconstruct a view-space position at integer texel `c` (clamped to the
// texture bounds — ClampToEdge equivalent for an integer load) from `depth`'s
// raw [0,1] value.
fn ssao_view_pos(
    depth: texture_2d<f32>,
    c: vec2<i32>,
    dims_i: vec2<i32>,
    tan_half_fov: f32,
    aspect: f32,
    near: f32,
    far: f32,
) -> vec3<f32> {
    let cc = clamp(c, vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));
    let raw = textureLoad(depth, cc, 0).r;
    let view_z = linearize_depth(raw, near, far);
    let uv = (vec2<f32>(cc) + vec2<f32>(0.5, 0.5)) / vec2<f32>(dims_i);
    let ndc_x = uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - uv.y * 2.0;
    let view_x = ndc_x * tan_half_fov * aspect * view_z;
    let view_y = ndc_y * tan_half_fov * view_z;
    return vec3<f32>(view_x, view_y, view_z);
}

fn body(
    depth: texture_2d<f32>,
    uv: vec2<f32>,
    dims: vec2<f32>,
    radius: f32,
    intensity: f32,
    bias: f32,
    fov_y: f32,
    near: f32,
    far: f32,
) -> vec4<f32> {
    let dims_i = vec2<i32>(dims);
    let c = vec2<i32>(uv * dims);
    let tan_half_fov = tan(fov_y * 0.5);
    let aspect = dims.x / dims.y;

    let p_c = ssao_view_pos(depth, c, dims_i, tan_half_fov, aspect, near, far);
    let p_xp = ssao_view_pos(depth, c + vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, near, far);
    let p_xm = ssao_view_pos(depth, c - vec2<i32>(1, 0), dims_i, tan_half_fov, aspect, near, far);
    let p_yp = ssao_view_pos(depth, c + vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, near, far);
    let p_ym = ssao_view_pos(depth, c - vec2<i32>(0, 1), dims_i, tan_half_fov, aspect, near, far);

    let ddx = p_xp - p_xm;
    let ddy = p_yp - p_ym;
    var normal = cross(ddx, ddy);
    let normal_len = length(normal);
    if normal_len > 1e-8 {
        normal = normal / normal_len;
    } else {
        normal = vec3<f32>(0.0, 0.0, -1.0);
    }

    let up_ref = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.999);
    let tangent = normalize(cross(up_ref, normal));
    let bitangent = cross(normal, tangent);

    let rot = ssao_hash_angle(vec2<f32>(c));

    var occlusion = 0.0;
    for (var i: u32 = 0u; i < SSAO_N; i = i + 1u) {
        let r = sqrt((f32(i) + 0.5) / f32(SSAO_N));
        let theta = f32(i) * SSAO_GOLDEN_ANGLE + rot;
        let disc_x = r * cos(theta);
        let disc_y = r * sin(theta);
        let disc_z = sqrt(max(0.0, 1.0 - r * r));
        let kernel_vec = tangent * disc_x + bitangent * disc_y + normal * disc_z;
        let sample_pos = p_c + kernel_vec * radius;

        let vz = max(sample_pos.z, 1e-4);
        let denom = tan_half_fov * vz;
        let sample_ndc_x = sample_pos.x / (aspect * denom);
        let sample_ndc_y = sample_pos.y / denom;
        let sample_uv = vec2<f32>(sample_ndc_x * 0.5 + 0.5, (1.0 - sample_ndc_y) * 0.5);
        let sample_c = clamp(vec2<i32>(sample_uv * dims), vec2<i32>(0, 0), dims_i - vec2<i32>(1, 1));

        let scene_raw = textureLoad(depth, sample_c, 0).r;
        let scene_view_z = linearize_depth(scene_raw, near, far);

        let occluded = select(0.0, 1.0, scene_view_z <= sample_pos.z - bias);
        let range_ok = select(0.0, 1.0, abs(p_c.z - scene_view_z) < radius);
        occlusion = occlusion + occluded * range_ok;
    }

    let ao = clamp(1.0 - intensity * occlusion / f32(SSAO_N), 0.0, 1.0);
    return vec4<f32>(ao, ao, ao, 1.0);
}
