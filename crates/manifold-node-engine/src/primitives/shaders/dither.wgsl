// node.dither — luminance-preserving dither quantize, driven by an external
// threshold pattern (node.dither_pattern, or any BYO pattern texture).
//
// Quantizes Rec.709 luminance to 8→2 levels as `amount` goes 0→1, dither-biased
// by the pattern's R channel, then preserves hue by scaling the original colour
// by the dithered/original luminance ratio, and crossfades against the source
// by `amount`. Math is verbatim from the legacy fused fx_dither (the only
// change: the threshold T is read from `pattern_tex` instead of computed
// inline). Pattern and source share dimensions, so textureLoad aligns exactly.

struct Uniforms {
    amount: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var pattern_tex: texture_2d<f32>;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let coord = vec2<i32>(id.xy);
    let src = textureLoad(source_tex, coord, 0);
    let original = src.rgb;

    let threshold = textureLoad(pattern_tex, coord, 0).r;

    let lum = dot(original, vec3<f32>(0.2126, 0.7152, 0.0722));
    let levels = mix(8.0, 2.0, uniforms.amount);

    var dithered = (lum + (threshold - 0.5) / levels) * levels;
    dithered = floor(dithered + 0.5) / levels;
    dithered = clamp(dithered, 0.0, 1.0);

    var scale: f32;
    if lum > 0.001 {
        scale = dithered / lum;
    } else {
        scale = dithered;
    }
    let dithered_color = original * scale;

    let result = mix(original, dithered_color, uniforms.amount);
    textureStore(output_tex, coord, vec4<f32>(result, src.a));
}
