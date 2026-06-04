// node.radial_offset_field — fusable body (freeze §12), SOURCE. Directional
// displacement-field generator. Radial mode: dir = normalize(uv-0.5) scaled by a
// centre→edge falloff mask (smoothstep(0,0.707,dist), faded by 1-falloff, near-
// centre fallback (1,0)). Linear mode: dir = (cos(angle), sin(angle)) uniform.
// Output (dir.x, dir.y, 0, 1), signed. Matches radial_offset_field.wgsl. PARAMS:
// [mode (Enum->u32), angle, falloff].
fn body(uv: vec2<f32>, dims: vec2<f32>, mode: u32, angle: f32, falloff: f32) -> vec4<f32> {
    var dir: vec2<f32>;
    if mode == 0u {
        let delta = uv - vec2<f32>(0.5, 0.5);
        let dist = length(delta);
        var radial_mask = smoothstep(0.0, 0.707, dist);
        radial_mask = mix(radial_mask, 1.0, 1.0 - falloff);
        if dist > 1e-5 {
            dir = normalize(delta) * radial_mask;
        } else {
            dir = vec2<f32>(1.0, 0.0);
        }
    } else {
        let rad = angle * 0.01745329;
        dir = vec2<f32>(cos(rad), sin(rad));
    }

    return vec4<f32>(dir, 0.0, 1.0);
}
