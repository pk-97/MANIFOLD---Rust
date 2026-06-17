// node.curl_slope_force_3d — fusable body (freeze §12), 3D-VOLUME CoincidentTexel.
// Combine a vec3 gradient Texture3D into a force field: cross the gradient with a
// curl-noise reference axis for swirl + add the gradient scaled by slope.
// `gradient` is read at the OWN voxel via integer textureLoad (CoincidentTexel, no
// sampler). `ref_axis` is normalized CPU-side in run() and supplies the base
// orientation; a smooth low-frequency spatial wobble (derived from `uv`, the
// normalized voxel centre) tilts it per-voxel so the swirl has no single global
// dead direction. Matches curl_slope_force_3d.wgsl. PARAMS: [vol_res, vol_depth
// (unused — guard is the wrapper's), curl_strength, slope_strength, ref_axis_x,
// ref_axis_y, ref_axis_z (pre-normalized)].
//
// Why the wobble: the swirl is cross(gradient, axis), whose magnitude is
// |gradient|·sin(angle between gradient and axis). A single fixed axis carves a
// quiet pole (gradient ∥ axis) and a hot equatorial belt, so curl energy pools in
// one octant of the volume — the 2D fluid sim avoids this because its swirl is a
// length-preserving rotation. Tilting the axis smoothly across space (low
// frequency → neighbouring voxels stay coherent, so this reads as real eddies, not
// white noise) gives every voxel a different dead direction, dissolving the global
// quiet pole into a thin broken surface. The axis stays unit-length, so curl
// magnitude still tracks curl_strength.
fn body(c_gradient: vec4<f32>, uv: vec3<f32>, dims: vec3<f32>, vol_res: i32, vol_depth: i32, curl_strength: f32, slope_strength: f32, ref_axis_x: f32, ref_axis_y: f32, ref_axis_z: f32) -> vec4<f32> {
    let gradient = c_gradient.xyz;
    let ref_axis = vec3<f32>(ref_axis_x, ref_axis_y, ref_axis_z);

    let tau = 6.2831853;
    let wob = vec3<f32>(
        sin(uv.y * tau * 2.0) + cos(uv.z * tau),
        sin(uv.z * tau * 2.0) + cos(uv.x * tau),
        sin(uv.x * tau * 2.0) + cos(uv.y * tau),
    );
    let axis_raw = ref_axis + wob * 0.9;
    let axis_len = length(axis_raw);
    let axis = select(ref_axis, axis_raw / axis_len, axis_len > 1e-4);

    let curl_force = cross(gradient, axis);
    let force = curl_force * curl_strength + gradient * slope_strength;
    return vec4<f32>(force, 0.0);
}
