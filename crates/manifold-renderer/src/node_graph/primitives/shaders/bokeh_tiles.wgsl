// Conservative CoC maxima in 8x8 half-resolution tiles (16x16 source pixels).
@group(0) @binding(0) var guide: texture_2d<f32>;
@group(0) @binding(1) var tiles: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(tiles)) { return; }
    let dims = textureDimensions(guide);
    var r = vec2<f32>(0.0);
    for (var y = 0u; y < 8u; y++) {
        for (var x = 0u; x < 8u; x++) {
            let p = min(id.xy * 8u + vec2<u32>(x,y), dims - 1u);
            r = max(r, textureLoad(guide, vec2<i32>(p), 0).rg);
        }
    }
    textureStore(tiles, vec2<i32>(id.xy), vec4<f32>(r, 0.0, 0.0));
}
