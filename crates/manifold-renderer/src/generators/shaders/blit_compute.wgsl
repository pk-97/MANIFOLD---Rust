// Compute variant of the BlitPipeline's inline blit shader.
// Simple texture passthrough: reads source with bilinear sampling,
// writes to output storage texture.

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let color = textureSampleLevel(t_source, s_source, uv, 0.0);
    textureStore(output_tex, vec2<i32>(gid.xy), color);
}
