// Mechanical port of Unity ColorGradeEffect.shader.
// K-matrix HSV, luma-based saturation, colorize pipeline, amount blend.

struct Uniforms {
    amount: f32,                // _Amount
    gain: f32,                  // _Gain
    saturation: f32,            // _Saturation
    hue: f32,                   // _Hue (degrees, -180..180)
    contrast: f32,              // _Contrast
    colorize: f32,              // _Colorize
    colorize_hue: f32,          // _ColorizeHue (degrees, 0..360)
    colorize_saturation: f32,   // _ColorizeSaturation
    colorize_focus: f32,        // _ColorizeFocus
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

// ColorGradeEffect.shader lines 66-74 — RGB → HSV (K-matrix method)
fn rgb_to_hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.b, c.g, K.w, K.z), vec4<f32>(c.g, c.b, K.x, K.y), step(c.b, c.g));
    let q = mix(vec4<f32>(p.x, p.y, p.w, c.r), vec4<f32>(c.r, p.y, p.z, p.x), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

// ColorGradeEffect.shader lines 77-82 — HSV → RGB (K-matrix method)
fn hsv_to_rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x, c.x, c.x) + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // ColorGradeEffect.shader lines 86-87
    let src = textureSample(source_tex, tex_sampler, in.uv);
    var c = src.rgb;

    // line 90: Gain (exposure)
    c *= uniforms.gain;

    // lines 93-94: Saturation — lerp toward luminance
    let luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    c = mix(vec3<f32>(luma, luma, luma), c, uniforms.saturation);

    // lines 98-103: Hue shift — rotate in HSV space (skip when hue ~0)
    if abs(uniforms.hue) > 0.01 {
        var hsv = rgb_to_hsv(c);
        hsv.x = fract(hsv.x + uniforms.hue / 360.0);
        c = hsv_to_rgb(hsv);
    }

    // line 106: Contrast — pivot around 0.5
    c = (c - 0.5) * uniforms.contrast + 0.5;

    // lines 110-128: Colorize pass
    let colorize = clamp(uniforms.colorize, 0.0, 1.0);
    if colorize > 1e-4 {
        // line 113: tint HSV → RGB
        let tint_h = fract(uniforms.colorize_hue / 360.0);
        let tint_hsv = vec3<f32>(
            tint_h,
            clamp(uniforms.colorize_saturation, 0.0, 1.0),
            1.0);
        let tint_rgb = hsv_to_rgb(tint_hsv);

        // lines 120-125: highlight/neutral masks
        let graded_luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
        let graded_sat = rgb_to_hsv(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))).y;
        let highlight_mask = smoothstep(0.18, 0.95, graded_luma);
        let neutral_mask = 1.0 - smoothstep(0.10, 0.80, graded_sat);
        let focus = clamp(uniforms.colorize_focus, 0.0, 1.0);
        let element_mask = mix(1.0, highlight_mask * neutral_mask, focus);

        // lines 126-127: tinted blend
        let tinted = tint_rgb * graded_luma;
        c = mix(c, tinted, colorize * element_mask);
    }

    // line 130: Final amount blend
    let result = mix(src.rgb, c, uniforms.amount);
    // Clamp to non-negative: contrast pivot can produce negative values for dark
    // pixels (e.g. (0 - 0.5) * 1.35 + 0.5 = −0.18). ACES tonemap maps negatives
    // to bright gray (~0.65), causing white-flash on frames with sparse content.
    return vec4<f32>(max(result, vec3<f32>(0.0)), src.a);
}
