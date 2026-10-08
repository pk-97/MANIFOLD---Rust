// node.smoothstep_texture — per-pixel WGSL smoothstep on RGB.
//
// out.rgb = smoothstep(low, high, in.rgb) per channel.
// out.a   = in.a (alpha pass-through).
//
// Hermite polynomial 3t²-2t³ where t = clamp((x - low) / (high - low), 0, 1).
// `low > high` produces an inverted curve (smoothstep flips signs).
//
// For a symmetric-around-zero band (the old `Mode = Bipolar` shortcut),
// wire `node.math(operation=Negate, in=high) → low` so a single `high`
// slider drives both edges.

struct Uniforms {
    low:   f32,
    high:  f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let s = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let out = vec4<f32>(
        smoothstep(u.low, u.high, s.r),
        smoothstep(u.low, u.high, s.g),
        smoothstep(u.low, u.high, s.b),
        s.a,
    );
    textureStore(output_tex, vec2<i32>(id.xy), out);
}
