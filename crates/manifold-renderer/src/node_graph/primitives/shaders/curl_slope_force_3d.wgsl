// node.curl_slope_force_3d — combine a vec3 gradient Texture3D into a
// force field: cross the gradient with a (unit) reference axis for curl
// (tangential orbit around density peaks) and add the gradient scaled by
// slope (radial push/pull). Writes a vec3 force Texture3D.
//
//   curl_force = cross(gradient, ref_axis)
//   force      = curl_force * curl_strength + gradient * slope_strength
//
// Bit-exact with the curl + slope half of the legacy fused
// fluid_gradient_curl_3d pass (the gradient half is the separate
// node.gradient_central_diff_3d atom upstream). `ref_axis` is normalized
// CPU-side before reaching the shader so curl magnitude tracks
// curl_strength exactly.

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

    // Curl: cross(gradient, ref_axis) — tangential flow around density peaks.
    let curl_force = cross(gradient, ref_axis);

    // Combined force: curl (tangential orbit) + slope (radial push/pull).
    let force = curl_force * params.curl_strength + gradient * params.slope_strength;

    textureStore(force_out, c, vec4<f32>(force, 0.0));
}
