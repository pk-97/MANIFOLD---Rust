// Wet/dry lerp shader — compute dispatch variant.
// Identical math to wet_dry_lerp.wgsl. Only the input/output mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

struct Uniforms {
    wet_dry: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_dry: texture_2d<f32>;
@group(0) @binding(2) var t_wet: texture_2d<f32>;
@group(0) @binding(3) var s: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let dry = textureSampleLevel(t_dry, s, uv, 0.0);
    let wet = textureSampleLevel(t_wet, s, uv, 0.0);
    let result = mix(dry, wet, u.wet_dry);
    textureStore(output_tex, vec2<i32>(gid.xy), result);
}
