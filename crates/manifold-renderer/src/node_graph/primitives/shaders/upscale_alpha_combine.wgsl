// BUG-om0v: MetalFX Temporal writes an opaque (or otherwise undefined) alpha
// into its output texture, so a raw blit of that output into the scene layer's
// `color` output propagated alpha 1.0 and blocked every layer beneath. This
// pass re-pairs the upscaler's RGB with the scene's own alpha, bilinearly
// upsampled from the render-res color scratch the upscaler read. Only the
// alpha channel is replaced; RGB is the upscaler's untouched.
//
// upscaled: MetalFX Temporal output at native res (Rgba16Float) — source of RGB.
// color_scratch: the render-res scene color scratch (Rgba16Float) — source of alpha.
// dst: the graph's native-res `color` output (Rgba16Float, storage-writeable).

@group(0) @binding(0) var upscaled: texture_2d<f32>;
@group(0) @binding(1) var color_scratch: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var dst: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(dst);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }
    let coord = vec2<i32>(gid.xy);
    // ClampToEdge sampler + (px + 0.5) / dims keeps every dst texel covered
    // regardless of the render/native size ratio; linear filtering upsamples
    // the render-res alpha to native res.
    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let upscaled_rgb = textureLoad(upscaled, coord, 0).rgb;
    let alpha = textureSampleLevel(color_scratch, samp, uv, 0.0).a;
    textureStore(dst, coord, vec4<f32>(upscaled_rgb, alpha));
}
