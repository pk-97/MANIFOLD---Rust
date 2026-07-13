// node.bokeh_gather — hand parity oracle for the generated standalone
// kernel (docs/CINEMATIC_POST_DESIGN.md D5, CINEMATIC_POST P4). Same
// 32-tap golden-angle disc gather as bokeh_gather_body.wgsl — kept
// independent (not sharing Rust source) so the gpu_tests parity check is a
// real cross-check, not a tautology.
//
// Bindings: uniforms(0), tex_in(1), tex_width(2), tex_sampler(3),
// output_tex(4, rgba16float storage) — matches the generated layout for a
// two-Gather-input, one-f32-param primitive (precedent:
// gaussian_blur_variable_width.wgsl).

struct Uniforms {
    max_radius: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex_in: texture_2d<f32>;
@group(0) @binding(2) var tex_width: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

const BOKEH_N: u32 = 32u;
const BOKEH_GOLDEN_ANGLE: f32 = 2.399963;

fn fetch_in(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_in, tex_sampler, uv, 0.0);
}

fn fetch_width(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(tex_width, tex_sampler, uv, 0.0);
}

fn bokeh_hash_angle(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453) * 6.283185307;
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims_i = textureDimensions(output_tex);
    if id.x >= u32(dims_i.x) || id.y >= u32(dims_i.y) {
        return;
    }

    let dims = vec2<f32>(f32(dims_i.x), f32(dims_i.y));
    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + vec2<f32>(0.5)) / dims;

    let center = fetch_in(uv);
    let center_coc_frac = clamp(fetch_width(uv).r, 0.0, 1.0);
    if center_coc_frac < 0.005 {
        textureStore(output_tex, vec2<i32>(id.xy), center);
        return;
    }

    let center_coc_px = center_coc_frac * u.max_radius;
    let texel = 1.0 / dims;
    let px = uv * dims;
    let rot = bokeh_hash_angle(px);

    var acc: vec3<f32> = vec3<f32>(0.0, 0.0, 0.0);
    var w_acc: f32 = 0.0;

    for (var i: u32 = 0u; i < BOKEH_N; i = i + 1u) {
        let r = sqrt((f32(i) + 0.5) / f32(BOKEH_N));
        let theta = f32(i) * BOKEH_GOLDEN_ANGLE + rot;
        let offset_px = vec2<f32>(r * cos(theta), r * sin(theta)) * center_coc_px;
        let tap_uv = uv + offset_px * texel;

        let tap_color = fetch_in(tap_uv).rgb;
        let tap_coc_px = clamp(fetch_width(tap_uv).r, 0.0, 1.0) * u.max_radius;
        let distance_to_center_px = length(offset_px);
        let w = step(distance_to_center_px, tap_coc_px);

        acc = acc + tap_color * w;
        w_acc = w_acc + w;
    }

    let rgb = select(center.rgb, acc / max(w_acc, 0.0001), w_acc > 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(rgb, center.a));
}
