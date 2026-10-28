// node.motion_blur pass 1: the longest smear in each TILE x TILE block of
// output pixels. Velocity is read at each pixel's spot in its own grid, so a
// smaller velocity buffer (temporal upscale) works unchanged.
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var velocity_tex: texture_2d<f32>;
@group(0) @binding(2) var tiles: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(tiles)) {
        return;
    }
    let vdims = textureDimensions(velocity_tex);
    let last = vec2<u32>(u.dims) - 1u;
    var best = vec2<f32>(0.0);
    var best_len2 = 0.0;
    for (var y = 0u; y < TILE; y++) {
        for (var x = 0u; x < TILE; x++) {
            let p = min(id.xy * TILE + vec2<u32>(x, y), last);
            let uv = (vec2<f32>(p) + 0.5) / u.dims;
            let ndc = textureLoad(velocity_tex, own_texel(uv, vdims), 0).rg;
            let s = spread(ndc, u.dims, u.shutter_scale, u.max_blur_px);
            let len2 = dot(s, s);
            if len2 > best_len2 {
                best = s;
                best_len2 = len2;
            }
        }
    }
    textureStore(tiles, vec2<i32>(id.xy), vec4<f32>(best, 0.0, 0.0));
}
