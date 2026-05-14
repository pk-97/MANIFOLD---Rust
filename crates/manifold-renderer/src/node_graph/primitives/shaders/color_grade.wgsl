// node.color_grade — pixel-exact replacement for legacy
// `effects/shaders/color_grade.wgsl`. Binding indices, math
// (K-matrix HSV, luma-based saturation, colorize pipeline), and
// dispatch shape preserved verbatim. Changing any of this breaks the
// parity test.
//
// 9 parameters in declaration order: amount, gain, saturation, hue,
// contrast, colorize, colorize_hue, colorize_saturation, colorize_focus.
// 12 bytes of trailing padding for 16-byte uniform alignment.

struct Uniforms {
    amount: f32,
    gain: f32,
    saturation: f32,
    hue: f32,
    contrast: f32,
    colorize: f32,
    colorize_hue: f32,
    colorize_saturation: f32,
    colorize_focus: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.b, c.g, K.w, K.z), vec4<f32>(c.g, c.b, K.x, K.y), step(c.b, c.g));
    let q = mix(vec4<f32>(p.x, p.y, p.w, c.r), vec4<f32>(c.r, p.y, p.z, p.x), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

fn hsv_to_rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x, c.x, c.x) + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    var c = src.rgb;

    c *= uniforms.gain;

    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = mix(vec3<f32>(luma, luma, luma), c, uniforms.saturation);

    if abs(uniforms.hue) > 0.01 {
        var hsv = rgb_to_hsv(c);
        hsv.x = fract(hsv.x + uniforms.hue / 360.0);
        c = hsv_to_rgb(hsv);
    }

    c = (c - 0.5) * uniforms.contrast + 0.5;

    let colorize = clamp(uniforms.colorize, 0.0, 1.0);
    if colorize > 1e-4 {
        let tint_h = fract(uniforms.colorize_hue / 360.0);
        let tint_hsv = vec3<f32>(
            tint_h,
            clamp(uniforms.colorize_saturation, 0.0, 1.0),
            1.0);
        let tint_rgb = hsv_to_rgb(tint_hsv);

        let graded_luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
        let graded_sat = rgb_to_hsv(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))).y;
        let highlight_mask = smoothstep(0.18, 0.95, graded_luma);
        let neutral_mask = 1.0 - smoothstep(0.10, 0.80, graded_sat);
        let focus = clamp(uniforms.colorize_focus, 0.0, 1.0);
        let element_mask = mix(1.0, highlight_mask * neutral_mask, focus);

        let tinted = tint_rgb * graded_luma;
        c = mix(c, tinted, colorize * element_mask);
    }

    let result = mix(src.rgb, c, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(max(result, vec3<f32>(0.0)), src.a));
}
