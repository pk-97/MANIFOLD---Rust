// node.sin_texture — per-pixel sin(input.rgb * freq + phase).
//
// Output channels:
//   R = sin(input.r * freq + phase)
//   G = sin(input.g * freq + phase)
//   B = sin(input.b * freq + phase)
//   A = input.a   (alpha passes through)
//
// Output range is [-1, 1] (raw sin). Chain through node.scale_offset_texture
// with scale=0.5, offset=0.5 to remap into [0, 1] for normal display.

struct Uniforms {
    freq:  f32,    // frequency multiplier on input value
    phase: f32,    // phase offset (radians)
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
        sin(s.r * u.freq + u.phase),
        sin(s.g * u.freq + u.phase),
        sin(s.b * u.freq + u.phase),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
