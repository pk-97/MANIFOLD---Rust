// node.gradient_central_diff_3d — fusable body (freeze §12), 3D-VOLUME
// GatherTexel. 6-tap central-difference gradient of a scalar density Texture3D.
// `density` is read via integer textureLoad at the 6 axis neighbours with toroidal
// wrap (XY use vol_res, Z uses vol_depth); gradient = (dx, dy, dz) * 0.5 in
// integer voxel space. The voxel coord is recovered from uv (= (id+0.5)/dims, so
// uv*dims truncates back to id). Matches gradient_central_diff_3d.wgsl. PARAMS:
// [vol_res (Int->i32), vol_depth (Int->i32)].
fn body(density: texture_3d<f32>, uv: vec3<f32>, dims: vec3<f32>, vol_res: i32, vol_depth: i32) -> vec4<f32> {
    let c = vec3<i32>(uv * dims);
    let ivr = vol_res;
    let idp = vol_depth;

    let dx = textureLoad(density, vec3<i32>(((c.x + 1) % ivr + ivr) % ivr, c.y, c.z), 0).r
           - textureLoad(density, vec3<i32>(((c.x - 1) % ivr + ivr) % ivr, c.y, c.z), 0).r;
    let dy = textureLoad(density, vec3<i32>(c.x, ((c.y + 1) % ivr + ivr) % ivr, c.z), 0).r
           - textureLoad(density, vec3<i32>(c.x, ((c.y - 1) % ivr + ivr) % ivr, c.z), 0).r;
    let dz = textureLoad(density, vec3<i32>(c.x, c.y, ((c.z + 1) % idp + idp) % idp), 0).r
           - textureLoad(density, vec3<i32>(c.x, c.y, ((c.z - 1) % idp + idp) % idp), 0).r;

    let gradient = vec3<f32>(dx, dy, dz) * 0.5;
    return vec4<f32>(gradient, 0.0);
}
