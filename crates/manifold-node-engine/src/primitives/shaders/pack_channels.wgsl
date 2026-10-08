// node.pack_channels — combine four single-channel textures into one
// RGBA output by reading the `.r` of each input into the corresponding
// output channel. Unwired inputs fall back to the `default_*` value.
//
// Bindings:
//   @binding(0) uniforms (32 bytes — 4 use flags + 4 defaults)
//   @binding(1) tex_r
//   @binding(2) tex_g
//   @binding(3) tex_b
//   @binding(4) tex_a
//   @binding(5) tex_sampler
//   @binding(6) output_tex (rgba16float storage)

struct Uniforms {
    use_r: u32,
    use_g: u32,
    use_b: u32,
    use_a: u32,
    defaults: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_r: texture_2d<f32>;
@group(0) @binding(2) var tex_g: texture_2d<f32>;
@group(0) @binding(3) var tex_b: texture_2d<f32>;
@group(0) @binding(4) var tex_a: texture_2d<f32>;
@group(0) @binding(5) var tex_sampler: sampler;
@group(0) @binding(6) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    var rgba = uniforms.defaults;
    if uniforms.use_r != 0u {
        rgba.r = textureSampleLevel(tex_r, tex_sampler, uv, 0.0).r;
    }
    if uniforms.use_g != 0u {
        rgba.g = textureSampleLevel(tex_g, tex_sampler, uv, 0.0).r;
    }
    if uniforms.use_b != 0u {
        rgba.b = textureSampleLevel(tex_b, tex_sampler, uv, 0.0).r;
    }
    if uniforms.use_a != 0u {
        rgba.a = textureSampleLevel(tex_a, tex_sampler, uv, 0.0).r;
    }
    textureStore(output_tex, vec2<i32>(id.xy), rgba);
}
