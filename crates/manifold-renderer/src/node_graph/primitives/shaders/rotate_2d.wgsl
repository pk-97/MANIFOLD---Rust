// node.rotate_2d — rotate a 2-channel coordinate texture by an angle.
//
//   out.r = in.r * cos(angle) - in.g * sin(angle)
//   out.g = in.r * sin(angle) + in.g * cos(angle)
//   out.b = 0
//   out.a = 1
//
// Operates on coordinate fields (R/G hold x/y), not pixel-sampled
// textures. Pair with node.centered_uv upstream and node.field_combine
// downstream to slice rotated x/y channels into scalar projections.

struct Uniforms {
    cos_a: f32,
    sin_a: f32,
    _pad0: f32,
    _pad1: f32,
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
    let rx = s.r * u.cos_a - s.g * u.sin_a;
    let ry = s.r * u.sin_a + s.g * u.cos_a;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rx, ry, 0.0, 1.0));
}
