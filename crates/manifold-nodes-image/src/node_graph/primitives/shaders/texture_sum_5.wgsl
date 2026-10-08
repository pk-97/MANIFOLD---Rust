// node.texture_sum_5 — per-pixel weighted-sum of 5 textures.
//
//   out = (a + b + c + d + e) / divisor   (per channel, alpha included)
//
// `divisor=1.0` (default) keeps the result a plain sum; `divisor=5.0`
// turns the op into an average (the natural shape for any
// "average of N component textures" pattern — Plasma's contrast curve,
// multi-tap blur, multi-band composition).

struct Uniforms {
    divisor: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var a_tex: texture_2d<f32>;
@group(0) @binding(2) var b_tex: texture_2d<f32>;
@group(0) @binding(3) var c_tex: texture_2d<f32>;
@group(0) @binding(4) var d_tex: texture_2d<f32>;
@group(0) @binding(5) var e_tex: texture_2d<f32>;
@group(0) @binding(6) var tex_sampler: sampler;
@group(0) @binding(7) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(a_tex, tex_sampler, uv, 0.0)
          + textureSampleLevel(b_tex, tex_sampler, uv, 0.0)
          + textureSampleLevel(c_tex, tex_sampler, uv, 0.0)
          + textureSampleLevel(d_tex, tex_sampler, uv, 0.0)
          + textureSampleLevel(e_tex, tex_sampler, uv, 0.0);
    let inv = select(1.0 / u.divisor, 0.0, abs(u.divisor) < 1e-9);
    textureStore(output_tex, vec2<i32>(id.xy), s * inv);
}
