// node.mix — unified two-texture compositor with 7 blend modes.
//
// Algorithm:
//   blended = blend(a, b, mode)
//   out.rgb = mix(a.rgb, blended, amount)
//   out.a   = mix(a.a, b.a, amount)   if mode == Lerp (0)
//           = a.a                     otherwise (BUG-181: blend modes are
//                                      RGB-only; `a` is the base/display
//                                      input and its alpha always survives)
//
// At mode = Lerp (0), blend(a,b) returns b, so the outer mix degenerates
// to a pure linear crossfade `mix(a, b, amount)` and `amount = 0.5` gives
// the half-average. At amount = 0 the RGB result is always `a` regardless
// of mode (the blend op is fully crossfaded out); alpha is `a.a` in every
// non-Lerp mode independent of `amount`.
//
// Mode indices (must match `MIX_MODES` in compose.rs):
//   0 = Lerp        — blend = b (outer mix becomes pure lerp)
//   1 = Screen      — blend = 1 - (1-a)(1-b)
//   2 = Add         — blend = a + b
//   3 = Max         — blend = max(a, b)
//   4 = Multiply    — blend = a * b
//   5 = Difference  — blend = abs(a - b)
//   6 = Overlay     — per-channel: a<0.5 → 2ab; else → 1-2(1-a)(1-b)
//   7 = Divide      — per-channel a/b, guarded: |b|<1e-6 → 0 (avoid NaN/Inf)
//
// Bindings:
//   @binding(0) uniforms (amount + mode + 8 bytes pad → 16-byte aligned)
//   @binding(1) tex_a
//   @binding(2) tex_b
//   @binding(3) tex_sampler
//   @binding(4) output_tex (rgba16float storage)

struct Uniforms {
    amount: f32,
    mode: u32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

fn overlay_channel(a: f32, b: f32) -> f32 {
    if a < 0.5 {
        return 2.0 * a * b;
    }
    return 1.0 - 2.0 * (1.0 - a) * (1.0 - b);
}

fn safe_div(a: f32, b: f32) -> f32 {
    if abs(b) < 1.0e-6 {
        return 0.0;
    }
    return a / b;
}

fn blend_rgb(a: vec3<f32>, b: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 0u: { return b; }
        case 1u: { return 1.0 - (1.0 - a) * (1.0 - b); }
        case 2u: { return a + b; }
        case 3u: { return max(a, b); }
        case 4u: { return a * b; }
        case 5u: { return abs(a - b); }
        case 6u: {
            return vec3<f32>(
                overlay_channel(a.x, b.x),
                overlay_channel(a.y, b.y),
                overlay_channel(a.z, b.z),
            );
        }
        case 7u: {
            return vec3<f32>(
                safe_div(a.x, b.x),
                safe_div(a.y, b.y),
                safe_div(a.z, b.z),
            );
        }
        default: { return b; }
    }
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let a = textureSampleLevel(tex_a, tex_sampler, uv, 0.0);
    let b = textureSampleLevel(tex_b, tex_sampler, uv, 0.0);

    let blended_rgb = blend_rgb(a.rgb, b.rgb, uniforms.mode);
    let out_rgb = mix(a.rgb, blended_rgb, uniforms.amount);
    // BUG-181: alpha only crossfades a->b in Lerp mode (a genuine crossfade,
    // alpha included). Every other blend mode is RGB-only and passes `a`'s
    // alpha through untouched, regardless of `amount` — otherwise a data
    // texture's filler alpha (e.g. an SSAO map's alpha=1) overwrites a
    // display chain's real alpha and flattens the frame opaque.
    var out_a: f32;
    if uniforms.mode == 0u {
        out_a = mix(a.a, b.a, uniforms.amount);
    } else {
        out_a = a.a;
    }
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(out_rgb, out_a));
}
