// node.color_lut — pixel-exact replacement for legacy
// `effects/shaders/fx_infrared.wgsl`. Maps luminance (BT.601) through
// a 1D LUT (stored as a Wx1 texture), with contrast adjustment and
// crossfade against the source. Bindings, math, sampler usage, and
// dispatch shape preserved verbatim.
//
// The LUT covers [0, lut_range] of luminance — the legacy effect
// uses range=2.0 so HDR values >1 extrapolate the palette naturally.
// Caller controls range via the `lut_range` uniform.

struct Uniforms {
    amount: f32,
    contrast: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var lut_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    let lum_raw = dot(src.rgb, vec3<f32>(0.299, 0.587, 0.114));
    let lum = max(0.0, (lum_raw - 0.5) * uniforms.contrast + 0.5);

    // `lum * 0.5` matches legacy fx_infrared.wgsl exactly: legacy LUT
    // covers [0, 2.0] luminance over UV [0, 1], so coord = lum / 2.0.
    // The factor is baked into the primitive because Infrared is the
    // only legacy consumer; other LUT users will write their own
    // wrapper preset that rescales luminance ahead of LUT1D.
    let lut_coord = clamp(lum * 0.5, 0.0, 1.0);
    let thermal = textureSampleLevel(lut_tex, tex_sampler, vec2<f32>(lut_coord, 0.5), 0.0).rgb;

    let result = mix(src.rgb, thermal, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
