// node.colorize — tint an image toward a chosen hue, with the tint
// strength masked per-pixel by (brightness × neutrality × focus). A
// selective duotone/colorize toward highlights. Verbatim port of the
// ColorGrade colorize pass (effects/shaders/color_grade.wgsl lines
// 110-128) so it can stand in for that section in a graph.

struct Uniforms {
    amount: f32,      // colorize strength [0,1]
    hue: f32,         // tint hue (degrees, 0..360)
    saturation: f32,  // tint saturation
    focus: f32,       // mask focus [0,1] — 0 = tint everything, 1 = highlights/neutrals only
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

// K-matrix HSV (matches ColorGrade exactly).
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

    let colorize = clamp(uniforms.amount, 0.0, 1.0);

    // Tint colour: HSV → RGB.
    let tint_h = fract(uniforms.hue / 360.0);
    let tint_hsv = vec3<f32>(tint_h, clamp(uniforms.saturation, 0.0, 1.0), 1.0);
    let tint_rgb = hsv_to_rgb(tint_hsv);

    // Highlight / neutral masks.
    let graded_luma = dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
    let graded_sat = rgb_to_hsv(clamp(c, vec3<f32>(0.0), vec3<f32>(1.0))).y;
    let highlight_mask = smoothstep(0.18, 0.95, graded_luma);
    let neutral_mask = 1.0 - smoothstep(0.10, 0.80, graded_sat);
    let focus = clamp(uniforms.focus, 0.0, 1.0);
    let element_mask = mix(1.0, highlight_mask * neutral_mask, focus);

    // Tinted blend.
    let tinted = tint_rgb * graded_luma;
    c = mix(c, tinted, colorize * element_mask);

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(c, src.a));
}
