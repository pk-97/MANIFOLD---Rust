// node.curl_slope_force_3d — fusable body (freeze §12), 3D-VOLUME CoincidentTexel.
// Combine a vec3 gradient Texture3D into a force field: cross the gradient with a
// curl-noise reference axis for swirl + add the gradient scaled by slope.
// `gradient` is read at the OWN voxel via integer textureLoad (CoincidentTexel, no
// sampler). `ref_axis` is normalized CPU-side in run() and supplies the axis for
// the whole volume — one global axis, matching the legacy fused
// fluid_gradient_curl_3d pass bit-for-bit. Matches curl_slope_force_3d.wgsl.
// PARAMS: [vol_res, vol_depth (unused — guard is the wrapper's), curl_strength,
// slope_strength, ref_axis_x, ref_axis_y, ref_axis_z (pre-normalized)].
//
// History: a per-voxel spatial wobble briefly tilted the axis here (added during
// decomposition to dissolve the "quiet pole" where gradient ∥ axis). It was
// position-frozen, and its sin terms vanish / cos terms peak at the volume
// corners, so all eight corners shared one fixed +diagonal axis at ~1.6× unit
// strength — a permanent swirl anomaly parked in one corner octant (the
// "top-right cube" bug, 2026-07-10). The legacy axis wanders with time
// (ref_axis is derived from t*0.3 upstream), so any anisotropy drifts instead
// of parking; that is the reference look. If dissolving the quiet pole is ever
// wanted, it must be time-varying and corner-degenerate-free, as a deliberate
// design change — not a shader patch.
fn body(c_gradient: vec4<f32>, uv: vec3<f32>, dims: vec3<f32>, vol_res: i32, vol_depth: i32, curl_strength: f32, slope_strength: f32, ref_axis_x: f32, ref_axis_y: f32, ref_axis_z: f32) -> vec4<f32> {
    let gradient = c_gradient.xyz;
    let axis = vec3<f32>(ref_axis_x, ref_axis_y, ref_axis_z);

    let curl_force = cross(gradient, axis);
    let force = curl_force * curl_strength + gradient * slope_strength;
    return vec4<f32>(force, 0.0);
}
