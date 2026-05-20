// node.trig_texture — per-pixel sin/cos/tan of (input.rgb * freq + phase).
//
// Output channels per `mode`:
//   Sin(0) — R = sin(in.r * freq + phase), G/B sim, A pass-through
//   Cos(1) — R = cos(in.r * freq + phase), G/B sim, A pass-through
//   Tan(2) — R = tan(in.r * freq + phase) (clamped to ±32 for finite output)
//
// Replaces the old standalone `node.sin_texture` and `node.cos_texture`
// — same primitive with a mode switch, so authors don't pick the wrong
// one or need to swap nodes when iterating.

struct Uniforms {
    freq:  f32,
    phase: f32,
    mode:  u32,   // 0 = Sin, 1 = Cos, 2 = Tan
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

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
    let out = vec4<f32>(
        trig(s.r * u.freq + u.phase, u.mode),
        trig(s.g * u.freq + u.phase, u.mode),
        trig(s.b * u.freq + u.phase, u.mode),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
