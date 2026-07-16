// node.mix — fusable body (freeze §12), MultiInputCoincident: inputs `a` and
// `b` are sampled at the SAME element. blend(a,b,mode) then crossfade by
// `amount`. BUG-181: alpha only crossfades a->b in Lerp mode (mode == 0) — a
// genuine crossfade, alpha included. Every other blend mode is RGB-only and
// passes `a`'s alpha through untouched regardless of `amount` (the faithful
// per-atom alpha the codegen must carry). Matches mix.wgsl. PARAMS order:
// [amount, mode]; mode is an Enum param, carried as u32.
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
            return vec3<f32>(safe_div(a.x, b.x), safe_div(a.y, b.y), safe_div(a.z, b.z));
        }
        default: { return b; }
    }
}

fn body(a: vec4<f32>, b: vec4<f32>, uv: vec2<f32>, dims: vec2<f32>, amount: f32, mode: u32) -> vec4<f32> {
    let blended = blend_rgb(a.rgb, b.rgb, mode);
    let out_rgb = mix(a.rgb, blended, amount);
    // Branchless on purpose: an `if` here compiles differently in fused vs
    // standalone kernel contexts (FMA regrouping) and broke the fp32
    // bit-exact proof (precision contract §7.1's "match the exact arithmetic
    // form" gotcha). mix(x, y, 0.0) = x + (y-x)*0.0 = x exactly, so t_a = 0
    // IS alpha pass-through, in the original instruction shape.
    let t_a = select(0.0, amount, mode == 0u);
    let out_a = mix(a.a, b.a, t_a);
    return vec4<f32>(out_rgb, out_a);
}
