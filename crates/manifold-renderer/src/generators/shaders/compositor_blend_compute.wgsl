// Compute variant of compositor_blend.wgsl — same blend logic, no TBDR tile overhead.
// Reads base + blend textures, writes to storage texture output.

struct Uniforms {
    blend_mode: u32,
    opacity: f32,
    translate_x: f32,
    translate_y: f32,
    scale_val: f32,
    rotation: f32,
    aspect_ratio: f32,
    _pad: f32, // keeps struct at 32 bytes — WGSL uniform structs must be multiples of 16 bytes
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

    let base = textureSampleLevel(t_base, samp, uv, 0.0);

    // Transform blend UVs (center → rotate → scale → translate → uncenter)
    var blend_uv = uv - vec2<f32>(0.5);
    blend_uv.x *= u.aspect_ratio;

    let cos_r = cos(u.rotation);
    let sin_r = sin(u.rotation);
    blend_uv = vec2<f32>(
        blend_uv.x * cos_r - blend_uv.y * sin_r,
        blend_uv.x * sin_r + blend_uv.y * cos_r,
    );

    blend_uv.x /= u.aspect_ratio;

    let s_val = max(u.scale_val, 0.01);
    blend_uv /= s_val;

    blend_uv -= vec2<f32>(u.translate_x, u.translate_y);
    blend_uv += vec2<f32>(0.5);

    var blend: vec4<f32>;
    if blend_uv.x < 0.0 || blend_uv.x > 1.0 || blend_uv.y < 0.0 || blend_uv.y > 1.0 {
        blend = vec4<f32>(0.0);
    } else {
        blend = textureSampleLevel(t_blend, samp, blend_uv, 0.0);
    }

    let ba = base.a;
    let bl_a = blend.a;
    let b = base.rgb;

    var f_val = blend.rgb;
    if u.blend_mode != 0u && u.blend_mode != 5u && u.blend_mode != 6u {
        if blend.a > 0.001 {
            f_val = blend.rgb / max(blend.a, 0.01);
        } else {
            f_val = vec3<f32>(0.0);
        }
    }

    var blended: vec3<f32>;

    switch u.blend_mode {
        case 0u: {
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
            let stencil_rgb = b * bl_a;
            let stencil_a = ba * bl_a;
            var stencil_result = vec4<f32>(stencil_rgb, stencil_a);
            stencil_result = clamp(mix(base, stencil_result, u.opacity), vec4<f32>(-100.0), vec4<f32>(100.0));
            textureStore(t_output, vec2<i32>(id.xy), stencil_result);
            return;
        }
        case 6u: {
            textureStore(t_output, vec2<i32>(id.xy), clamp(vec4<f32>(mix(b, f_val, u.opacity), 1.0), vec4<f32>(-100.0), vec4<f32>(100.0)));
            return;
        }
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
    // NaN propagation guard: prevent corrupt values from one layer contaminating output
    final_result = clamp(final_result, vec4<f32>(-100.0), vec4<f32>(100.0));
    textureStore(t_output, vec2<i32>(id.xy), final_result);
}
