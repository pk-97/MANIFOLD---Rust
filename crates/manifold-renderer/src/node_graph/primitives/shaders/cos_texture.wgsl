// node.cos_texture — per-pixel cos(input.rgb * freq + phase).
// Same shape as node.sin_texture; cos is offered as its own
// primitive so authors can reach for the function they want
// without computing a phase shift.

struct Uniforms {
    freq:  f32,
    phase: f32,
    _pad0: f32,
    _pad1: f32,
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
        cos(s.r * u.freq + u.phase),
        cos(s.g * u.freq + u.phase),
        cos(s.b * u.freq + u.phase),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
