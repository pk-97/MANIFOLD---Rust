// node.curl_slope_force_3d — fusable body (freeze §12), 3D-VOLUME CoincidentTexel.
// Combine a vec3 gradient Texture3D into a force field: cross the gradient with a
// (unit) reference axis for curl + add the gradient scaled by slope. `gradient` is
// read at the OWN voxel via integer textureLoad (CoincidentTexel, no sampler). The
// ref_axis is normalized CPU-side in run() (so curl magnitude tracks curl_strength
// and there is no GPU rsqrt), arriving here pre-unit. Matches curl_slope_force_3d
// .wgsl. PARAMS: [vol_res, vol_depth (unused — guard is the wrapper's), curl_
// strength, slope_strength, ref_axis_x, ref_axis_y, ref_axis_z (pre-normalized)].
fn body(c_gradient: vec4<f32>, uv: vec3<f32>, dims: vec3<f32>, vol_res: i32, vol_depth: i32, curl_strength: f32, slope_strength: f32, ref_axis_x: f32, ref_axis_y: f32, ref_axis_z: f32) -> vec4<f32> {
    let gradient = c_gradient.xyz;
    let ref_axis = vec3<f32>(ref_axis_x, ref_axis_y, ref_axis_z);

    let curl_force = cross(gradient, ref_axis);
    let force = curl_force * curl_strength + gradient * slope_strength;
    return vec4<f32>(force, 0.0);
}
