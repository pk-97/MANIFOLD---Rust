// Feedback effect — compute dispatch variant.
// Identical math to feedback.wgsl. Only the input/output mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment
//
// Bindings: uniform, source_tex, sampler, feedback_tex (previous frame), output_tex (write)

struct Uniforms {
    feedback_amount: f32,  // 0..1 — how much of previous frame to retain
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;       // current frame
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var feedback_tex: texture_2d<f32>;     // previous frame state
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let current = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let previous = textureSampleLevel(feedback_tex, tex_sampler, uv, 0.0);
    let result = mix(current, previous, uniforms.feedback_amount);

    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
