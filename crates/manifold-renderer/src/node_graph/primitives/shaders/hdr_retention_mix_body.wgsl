// node.hdr_retention_mix — fusable body (freeze §12), MultiInputCoincident:
// compressed + reference at the SAME uv. SDR body (<=1) from compressed; HDR
// portion (>1) lerps compressed<->reference by retention; alpha from compressed.
// Matches hdr_retention_mix.wgsl. PARAMS: [retention].
fn body(c: vec4<f32>, r: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, retention: f32) -> vec4<f32> {
    let sdr = min(c.rgb, vec3<f32>(1.0));
    let compressed_hdr = max(c.rgb - vec3<f32>(1.0), vec3<f32>(0.0));
    let reference_hdr = max(r.rgb - vec3<f32>(1.0), vec3<f32>(0.0));
    let retained_hdr = mix(compressed_hdr, reference_hdr, retention);
    return vec4<f32>(sdr + retained_hdr, c.a);
}
