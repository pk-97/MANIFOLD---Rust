// node.colorize — fusable body (freeze §12). Tint toward a hue, strength
// masked per-pixel by brightness * neutrality * focus. Pure; alpha passes
// through. Matches colorize.wgsl. PARAMS order: [amount, hue, saturation, focus].
//
// rgb2hsv/hsv2rgb are bit-identical to hue_saturation's (codegen dedups).
fn rgb2hsv(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(0.0, -1.0 / 3.0, 2.0 / 3.0, -1.0);
    let p = mix(vec4<f32>(c.b, c.g, K.w, K.z), vec4<f32>(c.g, c.b, K.x, K.y), step(c.b, c.g));
    let q = mix(vec4<f32>(p.x, p.y, p.w, c.r), vec4<f32>(c.r, p.y, p.z, p.x), step(p.x, c.r));
    let d = q.x - min(q.w, q.y);
    let e = 1.0e-10;
    return vec3<f32>(abs(q.z + (q.w - q.y) / (6.0 * d + e)), d / (q.x + e), q.x);
}

fn hsv2rgb(c: vec3<f32>) -> vec3<f32> {
    let K = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x, c.x, c.x) + K.xyz) * 6.0 - K.www);
    return c.z * mix(K.xxx, clamp(p - K.xxx, vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32, hue: f32, saturation: f32, focus: f32) -> vec4<f32> {
    let colorize = clamp(amount, 0.0, 1.0);
    let tint_h = fract(hue / 360.0);
    let tint_hsv = vec3<f32>(tint_h, clamp(saturation, 0.0, 1.0), 1.0);
    let tint_rgb = hsv2rgb(tint_hsv);
    let graded_luma = dot(c.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
    let graded_sat = rgb2hsv(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0))).y;
    let highlight_mask = smoothstep(0.18, 0.95, graded_luma);
    let neutral_mask = 1.0 - smoothstep(0.10, 0.80, graded_sat);
    let f = clamp(focus, 0.0, 1.0);
    let element_mask = mix(1.0, highlight_mask * neutral_mask, f);
    let tinted = tint_rgb * graded_luma;
    return vec4<f32>(mix(c.rgb, tinted, colorize * element_mask), c.a);
}
