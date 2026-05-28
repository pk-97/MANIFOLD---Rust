// Downsample source → analysis-sized RGBA8 staging texture for the
// BlobDetector FFI plugin. Bilinear sampling across the full source
// extent — replaces the previous `copy_texture_to_texture` blit
// which copied only the top-left analysis-sized patch, leaving the
// rest of the frame invisible to the plugin (and zero detections
// when the bright content lived off-center).

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var dst: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y { return; }
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let c = textureSampleLevel(src, src_sampler, uv, 0.0);
    textureStore(dst, vec2<i32>(gid.xy), c);
}
