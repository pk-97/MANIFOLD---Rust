// node.power_texture — per-pixel pow(max(input.rgb, 0), exponent).
//
// `max` with 0 protects against NaN when input is negative (pow of
// a negative base with non-integer exponent is undefined). Authors
// who need signed behavior should scale_offset to non-negative
// first.

struct Uniforms {
    exponent: f32,
    _pad0:    f32,
    _pad1:    f32,
    _pad2:    f32,
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
        pow(max(s.r, 0.0), u.exponent),
        pow(max(s.g, 0.0), u.exponent),
        pow(max(s.b, 0.0), u.exponent),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
