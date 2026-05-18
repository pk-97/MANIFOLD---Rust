// node.chroma_key — produce a mask from per-pixel colour proximity.
//
// Algorithm (per pixel):
//   dist = length(pixel.rgb - key_color)        // RGB Euclidean distance
//   raw  = 1.0 - smoothstep(tol - soft, tol + soft, dist)
//   mask = mix(raw, 1.0 - raw, invert)          // Select vs Reject
//
// At dist = 0 (exact colour match) raw = 1 (assuming tol > soft).
// At dist > tol + soft, raw = 0.
// Around `tolerance` the mask falls off smoothly over a band of
// width `2 * softness`.
//
// `invert` is an enum: 0 = Select (output 1 at match — default,
// the natural shape for "apply effect to this colour"), 1 = Reject
// (output 0 at match — traditional chroma-key/greenscreen shape).
//
// Output is written to all RGB channels so the mask is visible as
// grayscale when inspected directly, but downstream `masked_mix`
// (and any other mask consumer) reads only `.r`.
//
// Bindings:
//   @binding(0) uniforms
//   @binding(1) tex_in
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    key_r: f32,
    key_g: f32,
    key_b: f32,
    tolerance: f32,
    softness: f32,
    invert: u32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let pixel = textureSampleLevel(tex_in, tex_sampler, uv, 0.0);
    let key = vec3<f32>(uniforms.key_r, uniforms.key_g, uniforms.key_b);
    let dist = length(pixel.rgb - key);

    let edge_lo = uniforms.tolerance - uniforms.softness;
    let edge_hi = uniforms.tolerance + uniforms.softness;
    let raw = 1.0 - smoothstep(edge_lo, edge_hi, dist);
    var mask = raw;
    if uniforms.invert != 0u {
        mask = 1.0 - raw;
    }
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(mask, mask, mask, 1.0));
}
