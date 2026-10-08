// Compute compositor blend — reads base + blend textures, writes to storage output.
//
// Specialization axis (function constant via text replacement):
//   u.blend_mode  → 0u..12u  (dead-code eliminates inactive switch branches)
//
// Opaque (mode 6) eliminates the base texture read since the result is just
// the blend RGB with alpha = 1.

struct Uniforms {
    blend_mode: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_base: texture_2d<f32>;
@group(0) @binding(2) var t_blend: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var t_output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(t_base);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let blend = textureSampleLevel(t_blend, samp, uv, 0.0);

    // ── Opaque fast path ──────────────────────────────────────────
    // Opaque ignores base entirely — write blend RGB with alpha = 1.
    // When specialized, the compiler eliminates the base texture read below.
    if u.blend_mode == 6u {
        let b_rgb = blend.rgb;
        // Read base only if we need it for opacity mix
        if u.opacity < 1.0 {
            let base = textureSampleLevel(t_base, samp, uv, 0.0);
            textureStore(t_output, vec2<i32>(id.xy),
                clamp(vec4<f32>(mix(base.rgb, b_rgb, u.opacity), 1.0),
                      vec4<f32>(-100.0), vec4<f32>(100.0)));
        } else {
            textureStore(t_output, vec2<i32>(id.xy),
                clamp(vec4<f32>(b_rgb, 1.0),
                      vec4<f32>(-100.0), vec4<f32>(100.0)));
        }
        return;
    }

    // ── All other modes need the base texture ─────────────────────
    let base = textureSampleLevel(t_base, samp, uv, 0.0);
    let ba = base.a;
    let bl_a = blend.a;
    let b = base.rgb;

    // Unpremultiply blend for modes that need straight-alpha blending
    var f_val = blend.rgb;
    if u.blend_mode != 0u && u.blend_mode != 5u {
        if blend.a > 0.001 {
            f_val = blend.rgb / max(blend.a, 0.01);
        } else {
            f_val = vec3<f32>(0.0);
        }
    }

    var blended: vec3<f32>;

    switch u.blend_mode {
        case 0u: {
            // Normal — premultiplied alpha-over
            let out_a = bl_a + ba * (1.0 - bl_a);
            let out_rgb = f_val + b * (1.0 - bl_a);
            var result = vec4<f32>(out_rgb, out_a);
            result = clamp(mix(base, result, u.opacity), vec4<f32>(-100.0), vec4<f32>(100.0));
            textureStore(t_output, vec2<i32>(id.xy), result);
            return;
        }
        case 1u: { blended = b + f_val; }
        case 2u: { blended = b * f_val; }
        case 3u: {
            let sb = clamp(b, vec3<f32>(0.0), vec3<f32>(1.0));
            let sf = clamp(f_val, vec3<f32>(0.0), vec3<f32>(1.0));
            blended = vec3<f32>(1.0) - (vec3<f32>(1.0) - sb) * (vec3<f32>(1.0) - sf)
                    + max(vec3<f32>(0.0), b - vec3<f32>(1.0))
                    + max(vec3<f32>(0.0), f_val - vec3<f32>(1.0));
        }
        case 4u: {
            let sb = clamp(b, vec3<f32>(0.0), vec3<f32>(1.0));
            let sf = clamp(f_val, vec3<f32>(0.0), vec3<f32>(1.0));
            let lo = 2.0 * sb * sf;
            let hi = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - sb) * (vec3<f32>(1.0) - sf);
            blended = select(hi, lo, sb < vec3<f32>(0.5))
                    + max(vec3<f32>(0.0), b - vec3<f32>(1.0))
                    + max(vec3<f32>(0.0), f_val - vec3<f32>(1.0));
        }
        case 5u: {
            // Stencil — alpha mask
            let stencil_rgb = b * bl_a;
            let stencil_a = ba * bl_a;
            var stencil_result = vec4<f32>(stencil_rgb, stencil_a);
            stencil_result = clamp(mix(base, stencil_result, u.opacity), vec4<f32>(-100.0), vec4<f32>(100.0));
            textureStore(t_output, vec2<i32>(id.xy), stencil_result);
            return;
        }
        // case 6u handled above (opaque fast path)
        case 7u: { blended = abs(b - f_val); }
        case 8u: { blended = max(vec3<f32>(0.0), b + f_val - 2.0 * b * f_val); }
        case 9u: { blended = max(b - f_val, vec3<f32>(0.0)); }
        case 10u: {
            blended = vec3<f32>(
                select(b.r / (1.0 - f_val.r), 100.0, f_val.r >= 0.999),
                select(b.g / (1.0 - f_val.g), 100.0, f_val.g >= 0.999),
                select(b.b / (1.0 - f_val.b), 100.0, f_val.b >= 0.999),
            );
        }
        case 11u: { blended = max(b, f_val); }
        case 12u: { blended = min(b, f_val); }
        default: { blended = f_val; }
    }

    var out_rgb = mix(b, blended, bl_a);
    let out_a = bl_a + ba * (1.0 - bl_a);

    let blended_result = vec4<f32>(out_rgb, out_a);
    var final_result = mix(base, blended_result, u.opacity);
    // NaN propagation guard
    final_result = clamp(final_result, vec4<f32>(-100.0), vec4<f32>(100.0));
    textureStore(t_output, vec2<i32>(id.xy), final_result);
}
