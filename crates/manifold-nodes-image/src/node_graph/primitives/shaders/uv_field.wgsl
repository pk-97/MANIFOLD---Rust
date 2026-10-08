// node.uv_field — write per-pixel UV coordinates as R/G channels.
//
// R = u (0 at left, 1 at right)
// G = v (0 at top,  1 at bottom)
// B = 0
// A = 1
//
// Foundation primitive for procedural texture compositions. Most
// other field generators (distance_to_point, polar_field, etc.)
// derive from this by per-pixel math; alternatively they can be
// computed directly from gid.

@group(0) @binding(0) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(uv.x, uv.y, 0.0, 1.0));
}
