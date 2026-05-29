// node.threshold — soft-knee bright-pass extract.
//
// Keeps pixels whose max-component brightness exceeds `level`, with a smooth
// `softness` knee below it, scaling the colour by the response (hue-preserving:
// out = colour * response). Pixels below the knee go to black. The standard
// bloom / glow / highlight-extract prefilter.
//
// Verbatim port of the legacy bloom bright_prefilter response curve, with the
// node's `softness` param standing in for the bloom `knee` — so
// node.threshold(level=0.42, softness=0.24) reproduces the bloom prefilter.

struct Uniforms {
    level: f32,
    softness: f32,
    _pad0: f32,
    _pad1: f32,
}

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
    let c = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    let lum = max(c.r, max(c.g, c.b));
    let soft_start = u.level - u.softness;
    var t = clamp((lum - soft_start) / max(2.0 * u.softness, 1e-5), 0.0, 1.0);
    t = t * t * (3.0 - 2.0 * t);
    let hard = clamp((lum - u.level) / max(1.0 - u.level, 1e-5), 0.0, 1.0);
    let response = max(t * 0.78, hard);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(c.rgb * response, c.a));
}
