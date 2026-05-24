// node.levels — fused tone-shaping atom.
//
// Per pixel: out.rgb = pow(clamp(in.rgb * scale + offset, lo, hi), gamma);
//            out.a   = in.a (pass-through)
//
// Collapses the common `scale_offset_texture → clamp_texture → power_texture`
// cluster (3 dispatches, 3 intermediate WxH textures) into one. The shape
// covers MetallicGlass's height/metallic levels chains, Halation's bloom
// thresholds, OilyFluid's hue ramps — any per-channel affine + clamp + gamma
// in one shot.
//
// Bindings:
//   @binding(0) uniforms (32 bytes — scale + offset + lo + hi + gamma + 3 pad)
//   @binding(1) tex_source
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    scale: f32,
    offset: f32,
    lo: f32,
    hi: f32,
    gamma: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_source: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(tex_source, tex_sampler, uv, 0.0);

    let scaled = src.rgb * uniforms.scale + vec3<f32>(uniforms.offset);
    let clamped = clamp(scaled, vec3<f32>(uniforms.lo), vec3<f32>(uniforms.hi));
    // pow on a negative base is undefined; clamping above already guarantees
    // values are ≥ lo, so any preset that wants a fractional gamma should set
    // lo ≥ 0. Belt-and-braces: max with 0 before pow so we never feed pow a
    // negative value even if the caller mis-configured lo.
    let powered = pow(max(clamped, vec3<f32>(0.0)), vec3<f32>(uniforms.gamma));

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(powered, src.a));
}
