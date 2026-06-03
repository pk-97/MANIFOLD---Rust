// node.edge_detect — fusable body (freeze §12), GATHER. Sobel 3×3 on the
// luminance of `in` (gathered at the 8 neighbours, one texel apart) → gradient
// magnitude → smoothstep threshold → crossfade against the source by `amount`.
// The hand shader carries the texel step in its uniform (texel_size_x/y); the
// body recovers the same step from the ambient `dims` (output == source size for
// a 1:1 filter), so the generated kernel ignores those uniform fields. Matches
// edge_detect.wgsl. PARAMS: [amount, threshold].
fn edge_luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn body(in_tex: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, amount: f32, threshold: f32) -> vec4<f32> {
    let texel = vec2<f32>(1.0) / dims;
    let src = textureSampleLevel(in_tex, samp, uv, 0.0);

    let tl = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>(-1.0, -1.0) * texel, 0.0).rgb);
    let tc = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>( 0.0, -1.0) * texel, 0.0).rgb);
    let tr = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>( 1.0, -1.0) * texel, 0.0).rgb);
    let ml = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>(-1.0,  0.0) * texel, 0.0).rgb);
    let mr = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>( 1.0,  0.0) * texel, 0.0).rgb);
    let bl = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>(-1.0,  1.0) * texel, 0.0).rgb);
    let bc = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>( 0.0,  1.0) * texel, 0.0).rgb);
    let br = edge_luminance(textureSampleLevel(in_tex, samp, uv + vec2<f32>( 1.0,  1.0) * texel, 0.0).rgb);

    let gx = -tl - 2.0 * ml - bl + tr + 2.0 * mr + br;
    let gy = -tl - 2.0 * tc - tr + bl + 2.0 * bc + br;

    var edge = sqrt(gx * gx + gy * gy) * 0.25;
    edge = smoothstep(threshold * 0.5, threshold * 1.5 + 0.01, edge);

    let result = mix(src.rgb, vec3<f32>(edge), amount);
    return vec4<f32>(result, src.a);
}
