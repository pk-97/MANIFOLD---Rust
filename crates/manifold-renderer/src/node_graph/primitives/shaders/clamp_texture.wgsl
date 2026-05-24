// node.clamp_texture — saturate RGB to [min, max], alpha pass-through.
//
// Per pixel: out.rgb = clamp(in.rgb, min, max); out.a = in.a.
//
// Bindings:
//   @binding(0) uniforms (16 bytes — min + max + 2 pad)
//   @binding(1) tex_source
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    min: f32,
    max: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);
    let clamped = clamp(src.rgb, vec3<f32>(uniforms.min), vec3<f32>(uniforms.max));
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(clamped, src.a));
}
