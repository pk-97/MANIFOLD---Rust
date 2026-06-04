// node.trig_texture — fusable body (freeze §12), MultiInputCoincident with
// OPTIONAL-INPUT use-flags. Per-pixel sin/cos/tan of (in.rgb * freq + phase). freq
// and phase come from the scalar params unless their texture-shadow inputs
// (freq_tex / phase_tex) are wired, in which case each pixel reads them from the
// .r of the corresponding texture (use_*_tex flag, injected by the codegen for the
// optional inputs; unwired ones bind a dummy and the pre-read is discarded). A is
// passed through. Matches trig_texture.wgsl. PARAMS: [freq, phase, mode
// (Enum->u32)] + injected use_freq_tex/use_phase_tex.
fn tt_trig(x: f32, mode: u32) -> f32 {
    if mode == 0u { return sin(x); }
    if mode == 1u { return cos(x); }
    let t = tan(x);
    return clamp(t, -32.0, 32.0);
}

fn body(c_in: vec4<f32>, c_freq_tex: vec4<f32>, c_phase_tex: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, freq: f32, phase: f32, mode: u32, use_freq_tex: u32, use_phase_tex: u32) -> vec4<f32> {
    let s = c_in;

    var f = freq;
    if use_freq_tex == 1u {
        f = c_freq_tex.r;
    }
    var p = phase;
    if use_phase_tex == 1u {
        p = c_phase_tex.r;
    }

    return vec4<f32>(
        tt_trig(s.r * f + p, mode),
        tt_trig(s.g * f + p, mode),
        tt_trig(s.b * f + p, mode),
        s.a,
    );
}
