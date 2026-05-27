// node.trig_texture — per-pixel sin/cos/tan of (input.rgb * freq + phase).
//
// Output channels per `mode`:
//   Sin(0) — R = sin(in.r * freq + phase), G/B sim, A pass-through
//   Cos(1) — R = cos(in.r * freq + phase), G/B sim, A pass-through
//   Tan(2) — R = tan(in.r * freq + phase) (clamped to ±32 for finite output)
//
// `freq` and `phase` are scalar uniforms by default, port-shadowed by
// optional scalar inputs of the same name. They can ALSO be optionally
// driven per-pixel from texture inputs (`freq_tex` / `phase_tex`) — when
// wired, each pixel reads its freq / phase from the R channel of the
// corresponding texture, instead of the uniform value. This unlocks
// per-cell unique trig modulation patterns (per-star twinkle, cellular
// flicker, foam pop timing, etc.) when the freq / phase texture comes
// from a per-cell hash source like node.voronoi_2d (A channel) routed
// through node.channel_mix.
//
// Texture-shadow precedence: per-pixel texture > port-shadow scalar >
// param default. Texture-shadow flag flips per-channel in the shader.

struct Uniforms {
    freq:           f32,
    phase:          f32,
    mode:           u32,   // 0 = Sin, 1 = Cos, 2 = Tan
    use_freq_tex:   u32,   // 0 = scalar freq, 1 = per-pixel freq from freq_tex.r
    use_phase_tex:  u32,   // 0 = scalar phase, 1 = per-pixel phase from phase_tex.r
    _pad0:          u32,
    _pad1:          u32,
    _pad2:          u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var freq_tex: texture_2d<f32>;
@group(0) @binding(5) var phase_tex: texture_2d<f32>;

fn trig(x: f32, mode: u32) -> f32 {
    if mode == 0u { return sin(x); }
    if mode == 1u { return cos(x); }
    // Tan with clamp — pure tan() near π/2 blows to ±∞ which downstream
    // shaders propagate as NaN; clamping to ±32 keeps the output finite.
    let t = tan(x);
    return clamp(t, -32.0, 32.0);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);

    var freq = u.freq;
    if u.use_freq_tex == 1u {
        freq = textureSampleLevel(freq_tex, tex_sampler, uv, 0.0).r;
    }
    var phase = u.phase;
    if u.use_phase_tex == 1u {
        phase = textureSampleLevel(phase_tex, tex_sampler, uv, 0.0).r;
    }

    let out = vec4<f32>(
        trig(s.r * freq + phase, u.mode),
        trig(s.g * freq + phase, u.mode),
        trig(s.b * freq + phase, u.mode),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
