// node.field_combine — per-pixel scalar field from a linear
// combination of the input texture's R and G channels plus a constant.
//
// out.r = out.g = out.b = a * in.r + b * in.g + c
// out.a = 1.0
//
// The output is a scalar field broadcast to RGB so downstream
// per-pixel math primitives (sin_texture, scale_offset_texture,
// distance_to_point comparisons, …) can read it from any channel.
// `c` lets the linear combination include a constant offset without a
// separate scale_offset_texture node downstream — useful for
// centering uv on the fly (e.g. c = -0.5 * (a + b) recenters a
// `uv_field`-style (0..1) input around 0).

struct Uniforms {
    a: f32,
    b: f32,
    c: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let v = u.a * s.r + u.b * s.g + u.c;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(v, v, v, 1.0));
}
