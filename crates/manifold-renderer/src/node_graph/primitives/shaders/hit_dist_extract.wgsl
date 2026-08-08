// RAYTRACING_DESIGN.md section 17.5 DN-E (DN4): extract specular hit-distance
// from the upsampled reflection texture's .a channel into a single-channel R16Float
// texture for the MetalFX Temporal Denoised Scaler's specular hit-distance input.
//
// src: the full-res upsampled reflection texture (Rgba16Float), .a = hit distance
//      in world units from the reflection trace kernel, upsampled through the
//      same depth-aware bilateral filter as the reflection lighting.
// dst: the denoise_hit_dist_full texture (R16Float), populated only when
//      rt_denoise_feed is on.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var dst: texture_storage_2d<r16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(src);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let hit = textureLoad(src, vec2<i32>(gid.xy), 0).a;
    textureStore(dst, vec2<i32>(gid.xy), vec4<f32>(hit));
}
