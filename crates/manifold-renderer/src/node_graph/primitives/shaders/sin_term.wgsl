// node.sin_term — fused linear-projection + sin term.
//
//   proj = a * field.r + b * field.g + c
//   out  = sin(proj * freq * freq_scale + time * time_scale)
//
// The `field_combine → sin_term` cluster collapsed into one node — the
// natural shape for one term of any sum-of-sines pattern (Plasma's five
// summed sines, moiré, parametric standing waves).
//
// `field` is any 2-channel Texture2D — typically a coordinate texture
// from `node.centered_uv` (R = x, G = y) for linear projections, or a
// pre-computed scalar field from `node.distance_to_point` (R=G=B=value)
// for non-linear projections where defaults (a=1, b=0, c=0) read the
// broadcast R channel directly.

struct Uniforms {
    a:          f32,
    b:          f32,
    c:          f32,
    freq:       f32,
    freq_scale: f32,
    time:       f32,
    time_scale: f32,
    _pad0:      f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let field = u.a * s.r + u.b * s.g + u.c;
    let phase = u.time * u.time_scale;
    let freq = u.freq * u.freq_scale;
    let v = sin(field * freq + phase);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(v, v, v, 1.0));
}
