// Linear Rgba16Float → sRGB Bgra8Unorm conversion for live recording.
// Reads the compositor output (linear light, post-tonemap) and writes
// sRGB-gamma-corrected output for H.264 encoding.

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var output_tex: texture_storage_2d<bgra8unorm, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(t_source);
    if gid.x >= dims.x || gid.y >= dims.y {
        return;
    }

    var c = textureLoad(t_source, vec2<i32>(gid.xy), 0);

    // Linear → sRGB gamma (same as the Obj-C kCopyShaderSDR).
    c = vec4<f32>(
        pow(max(c.r, 0.0), 1.0 / 2.2),
        pow(max(c.g, 0.0), 1.0 / 2.2),
        pow(max(c.b, 0.0), 1.0 / 2.2),
        c.a,
    );

    textureStore(output_tex, vec2<i32>(gid.xy), c);
}
