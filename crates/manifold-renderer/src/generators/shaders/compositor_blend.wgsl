struct Uniforms {
    blend_mode: u32,
    opacity: f32,
    translate_x: f32,
    translate_y: f32,
    scale_val: f32,
    rotation: f32,
    aspect_ratio: f32,
    invert_colors: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_base: texture_2d<f32>;
@group(0) @binding(2) var t_blend: texture_2d<f32>;
@group(0) @binding(3) var s: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let base = textureSample(t_base, s, in.uv);

    // Transform blend UVs (center → rotate → scale → translate → uncenter)
    var blend_uv = in.uv - vec2<f32>(0.5);
    blend_uv.x *= u.aspect_ratio;

    // Rotate
    let cos_r = cos(u.rotation);
    let sin_r = sin(u.rotation);
    blend_uv = vec2<f32>(
        blend_uv.x * cos_r - blend_uv.y * sin_r,
        blend_uv.x * sin_r + blend_uv.y * cos_r,
    );

    blend_uv.x /= u.aspect_ratio;

    // Scale
    let s_val = max(u.scale_val, 0.01);
    blend_uv /= s_val;

    // Translate
    blend_uv -= vec2<f32>(u.translate_x, u.translate_y);
    blend_uv += vec2<f32>(0.5);

    // Bounds check — outside the source is transparent
    var blend: vec4<f32>;
    if blend_uv.x < 0.0 || blend_uv.x > 1.0 || blend_uv.y < 0.0 || blend_uv.y > 1.0 {
        blend = vec4<f32>(0.0);
    } else {
        blend = textureSample(t_blend, s, blend_uv);
    }

    let ba = base.a;
    let bl_a = blend.a;
    let b = base.rgb;

    // Unpremultiply blend for non-Normal/Stencil/Opaque blends
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
        // 0: Normal — standard alpha compositing
        case 0u: {
            blended = f_val;
        }
        // 1: Additive
        case 1u: {
            blended = b + f_val;
        }
        // 2: Multiply
        case 2u: {
            blended = b * f_val;
        }
        // 3: Screen
        case 3u: {
            blended = b + f_val - b * f_val;
        }
        // 4: Overlay
        case 4u: {
            let lo = 2.0 * b * f_val;
            let hi = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - b) * (vec3<f32>(1.0) - f_val);
            blended = select(hi, lo, b <= vec3<f32>(0.5));
        }
        // 5: Stencil — blend texture's alpha masks the base
        case 5u: {
            blended = b;
            let out_a = ba * bl_a * u.opacity;
            return vec4<f32>(b * out_a, out_a);
        }
        // 6: Opaque — fully replace, ignore alpha
        case 6u: {
            return vec4<f32>(mix(b, f_val, u.opacity), 1.0);
        }
        // 7: Difference
        case 7u: {
            blended = abs(b - f_val);
        }
        // 8: Exclusion
        case 8u: {
            blended = b + f_val - 2.0 * b * f_val;
        }
        // 9: Subtract
        case 9u: {
            blended = max(b - f_val, vec3<f32>(0.0));
        }
        // 10: ColorDodge
        case 10u: {
            blended = vec3<f32>(
                select(min(b.r / max(1.0 - f_val.r, 0.001), 1.0), 1.0, f_val.r >= 1.0),
                select(min(b.g / max(1.0 - f_val.g, 0.001), 1.0), 1.0, f_val.g >= 1.0),
                select(min(b.b / max(1.0 - f_val.b, 0.001), 1.0), 1.0, f_val.b >= 1.0),
            );
        }
        // 11: Lighten
        case 11u: {
            blended = max(b, f_val);
        }
        // 12: Darken
        case 12u: {
            blended = min(b, f_val);
        }
        default: {
            blended = f_val;
        }
    }

    // Alpha compositing: lerp base → blended by blend alpha
    var out_rgb = mix(b, blended, bl_a);
    let out_a = bl_a + ba * (1.0 - bl_a);

    // Post-blend invert (matches Unity: applied to composited result)
    if u.invert_colors > 0.5 {
        out_rgb = max(vec3<f32>(1.0) - out_rgb, vec3<f32>(0.0));
    }

    // Post-blend opacity lerp (matches Unity: lerp(base, result, opacity))
    let blended_result = vec4<f32>(out_rgb, out_a);
    let final_result = mix(base, blended_result, u.opacity);
    return final_result;
}
