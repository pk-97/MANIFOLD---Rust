// node.masked_mix — three-texture blend with a per-pixel mask weight.
//
// Algorithm:
//   weight = clamp(mask.r * amount, 0.0, 1.0)
//   out    = mix(a, b, weight)
//
// At weight = 0 the output is `a` unchanged; at weight = 1 it's `b`.
// The mask's red channel is the per-pixel blend weight — by convention
// luma_key / chroma_key / threshold all write their result to .r so
// any of them wires cleanly into this primitive's `mask` input.
//
// The scalar `amount` scales the mask globally — at amount = 0 the
// output is always `a` regardless of the mask, which makes this
// primitive's outer behaviour identical to `mix` when no mask is
// wired (and saves a separate "is masked?" branch in the runtime).
//
// Bindings:
//   @binding(0) uniforms (amount + 12 bytes pad → 16-byte aligned)
//   @binding(1) tex_a    — the "off" side (mask = 0)
//   @binding(2) tex_b    — the "on" side  (mask = 1)
//   @binding(3) tex_mask — per-pixel weight, .r channel
//   @binding(4) tex_sampler
//   @binding(5) output_tex (rgba16float storage)

struct Uniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_mask: texture_2d<f32>;
@group(0) @binding(4) var tex_sampler: sampler;
@group(0) @binding(5) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let a = textureSampleLevel(tex_a, tex_sampler, uv, 0.0);
    let b = textureSampleLevel(tex_b, tex_sampler, uv, 0.0);
    let m = textureSampleLevel(tex_mask, tex_sampler, uv, 0.0).r;
    let weight = clamp(m * uniforms.amount, 0.0, 1.0);
    let result = mix(a, b, weight);
    textureStore(output_tex, vec2<i32>(id.xy), result);
}
