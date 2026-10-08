// node.wet_dry — full-RGBA linear interpolation between a
// `dry` and `wet` texture. Pixel-exact port of legacy
// `effects/shaders/wet_dry_lerp_compute.wgsl`. Used by every composite
// that needs to crossfade a processed result back over its source
// (Bloom, Halation, Watercolor finals).
//
// out = mix(dry, wet, wet_dry)
//
// Bindings (canonical two-texture-input layout, identical to mix.wgsl):
//   @binding(0) uniforms (wet_dry + 12 bytes pad → 16-byte aligned)
//   @binding(1) tex_dry
//   @binding(2) tex_wet
//   @binding(3) tex_sampler
//   @binding(4) output_tex (rgba16float storage)

struct Uniforms {
    wet_dry: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_dry: texture_2d<f32>;
@group(0) @binding(2) var tex_wet: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let dry = textureSampleLevel(tex_dry, tex_sampler, uv, 0.0);
    let wet = textureSampleLevel(tex_wet, tex_sampler, uv, 0.0);
    let result = mix(dry, wet, uniforms.wet_dry);
    textureStore(output_tex, vec2<i32>(id.xy), result);
}
