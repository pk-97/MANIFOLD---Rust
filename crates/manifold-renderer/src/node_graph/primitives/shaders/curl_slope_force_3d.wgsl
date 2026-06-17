// node.curl_slope_force_3d — combine a vec3 gradient Texture3D into a
// force field: cross the gradient with a curl-noise reference axis for
// swirl (tangential orbit around density peaks) and add the gradient
// scaled by slope (radial push/pull). Writes a vec3 force Texture3D.
//
//   axis       = normalize(ref_axis + smooth_spatial_wobble(uv))
//   curl_force = cross(gradient, axis)
//   force      = curl_force * curl_strength + gradient * slope_strength
//
// Parity oracle for the generated standalone kernel (the body lives in
// curl_slope_force_3d_body.wgsl; the gradient half is the separate
// node.gradient_central_diff_3d atom upstream). `ref_axis` is normalized
// CPU-side and supplies the base orientation; the per-voxel wobble tilts
// it smoothly across space so the swirl has no single global dead
// direction. This DIVERGES from the legacy fused fluid_gradient_curl_3d
// pass, which used one fixed axis and so pooled curl energy in one octant
// (the swirl magnitude vanished where gradient ∥ ref_axis); see the body
// for the full rationale.

struct U {
    vol_res:        u32,
    vol_depth:      u32,
    _pad0:          u32,
    _pad1:          u32,
    curl_strength:  f32,
    slope_strength: f32,
    ref_axis_x:     f32,
    ref_axis_y:     f32,
    ref_axis_z:     f32,
    _pad2:          f32,
    _pad3:          f32,
    _pad4:          f32,
};

@group(0) @binding(0) var<uniform> params: U;
@group(0) @binding(1) var gradient_in: texture_3d<f32>;
@group(0) @binding(2) var force_out: texture_storage_3d<rgba16float, write>;

@compute @workgroup_size(8, 8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let vr    = params.vol_res;
    let depth = params.vol_depth;
    if id.x >= vr || id.y >= vr || id.z >= depth {
        return;
    }

    let c = vec3<i32>(i32(id.x), i32(id.y), i32(id.z));
    let gradient = textureLoad(gradient_in, c, 0).xyz;

    let ref_axis = vec3<f32>(params.ref_axis_x, params.ref_axis_y, params.ref_axis_z);

    // Curl-noise axis: tilt ref_axis by a smooth low-frequency spatial wobble so
    // the swirl has no single global dead direction (cross magnitude is
    // |gradient|·sin(angle to axis), which would otherwise carve a quiet pole +
    // hot belt). `uv` is the normalized voxel centre — matches the generated
    // kernel's (vec3<f32>(id) + 0.5) / vec3<f32>(textureDimensions(dst)).
    let uv = (vec3<f32>(id) + vec3<f32>(0.5)) / vec3<f32>(f32(vr), f32(vr), f32(depth));
    let tau = 6.2831853;
    let wob = vec3<f32>(
        sin(uv.y * tau * 2.0) + cos(uv.z * tau),
        sin(uv.z * tau * 2.0) + cos(uv.x * tau),
        sin(uv.x * tau * 2.0) + cos(uv.y * tau),
    );
    let axis_raw = ref_axis + wob * 0.9;
    let axis_len = length(axis_raw);
    let axis = select(ref_axis, axis_raw / axis_len, axis_len > 1e-4);

    // Curl: cross(gradient, axis) — tangential flow around density peaks.
    let curl_force = cross(gradient, axis);

    // Combined force: curl (tangential orbit) + slope (radial push/pull).
    let force = curl_force * params.curl_strength + gradient * params.slope_strength;

    textureStore(force_out, c, vec4<f32>(force, 0.0));
}
