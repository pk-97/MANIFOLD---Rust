// node.person_segment — bilinear-upsample the analysis-resolution
// person-mask staging texture into the runtime-allocated output
// Texture2D. Mask probability in R; G/B/A follow for downstream
// compatibility (matches depth_estimate_midas's pack convention so
// person_segment.out can drop into the same downstream slots —
// mix mask input, masked_mix, channel_mix, etc.).

@group(0) @binding(0) var mask_src: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let m = textureSampleLevel(mask_src, tex_sampler, uv, 0.0).r;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(m, m, m, 1.0));
}
