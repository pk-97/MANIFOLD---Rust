// node.fract_texture — per-pixel fract(input.rgb * scale).
//
// fract returns the fractional part: x - floor(x). Multiplying by
// scale before fract is the classic "tile a smooth field into N
// stripes" trick.

struct Uniforms {
    scale: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
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
    let out = vec4<f32>(
        fract(s.r * u.scale),
        fract(s.g * u.scale),
        fract(s.b * u.scale),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
