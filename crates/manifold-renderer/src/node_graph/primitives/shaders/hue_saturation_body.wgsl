// node.hue_saturation — fusable body (freeze §12). HSV rotate/scale: rotate
// hue (degrees), scale saturation + value in HSV space. Pure; alpha passes
// through. Matches hue_saturation.wgsl. PARAMS order: [hue, saturation, value].
//
// rgb2hsv/hsv2rgb (Sam Hocevar branchless) are bit-identical to colorize's;
// the fusion codegen content-dedups identical helpers when both atoms fuse.
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

fn body(c: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, hue: f32, saturation: f32, value: f32) -> vec4<f32> {
    var hsv = rgb2hsv(max(c.rgb, vec3<f32>(0.0)));
    hsv.x = fract(hsv.x + hue / 360.0);
    hsv.y = clamp(hsv.y * saturation, 0.0, 1.0);
    hsv.z = hsv.z * value;
    return vec4<f32>(hsv2rgb(hsv), c.a);
}
