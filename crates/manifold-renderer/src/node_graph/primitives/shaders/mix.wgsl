// primitive.mix — linear crossfade A → B.
//
// out = mix(a, b, amount).
//
// Bindings (canonical layout for two-texture-input primitives):
//   @binding(0) uniforms (amount + 12 bytes pad → 16-byte aligned)
//   @binding(1) tex_a
//   @binding(2) tex_b
//   @binding(3) tex_sampler
//   @binding(4) output_tex (rgba16float storage)

struct Uniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let a = textureSampleLevel(tex_a, tex_sampler, uv, 0.0);
    let b = textureSampleLevel(tex_b, tex_sampler, uv, 0.0);
    let result = mix(a, b, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), result);
}
