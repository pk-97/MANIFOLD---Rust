// Keep original sample radii separate from conservative search bounds.
@group(0) @binding(0) var guide: texture_2d<f32>;
@group(0) @binding(1) var reach: texture_2d<f32>;
@group(0) @binding(2) var packed: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16,16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= textureDimensions(packed)) { return; }
    let r = textureLoad(guide, vec2<i32>(id.xy), 0).rg;
    let tile = textureLoad(reach, vec2<i32>(id.xy / 8u), 0).rg;
    textureStore(packed, vec2<i32>(id.xy), vec4<f32>(r, tile));
}
