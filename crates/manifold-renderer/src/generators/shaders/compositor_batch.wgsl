// Compute batch blender — composites N consecutive effect-free layers in a
// single dispatch using bindless texture arrays. Replaces N separate blend
// render passes with 1 compute dispatch.
//
// Blend mode math is identical to compositor_blend.wgsl (same 13 modes,
// same UV transform, same alpha compositing).

struct LayerDesc {
    texture_index: u32,
    blend_mode: u32,
    opacity: f32,
    translate_x: f32,
    translate_y: f32,
    scale_val: f32,
    rotation: f32,
    aspect_ratio: f32,
    invert_colors: f32,
    _pad: f32,
    _pad1: f32,
    _pad2: f32,
};

struct BatchParams {
    layer_count: u32,
    width: u32,
    height: u32,
    _pad: u32,
};

// Group 0: binding array + textures + storage (no uniform allowed with binding_array)
@group(0) @binding(0) var base_tex: texture_2d<f32>;
@group(0) @binding(1) var tex_sampler: sampler;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(3) var<storage, read> layers: array<LayerDesc>;
@group(0) @binding(4) var layer_textures: binding_array<texture_2d<f32>>;

// Group 1: uniform params (separate group — wgpu disallows uniform + binding_array in same group)
@group(1) @binding(0) var<uniform> params: BatchParams;

// Transform UV for a layer (center → rotate → scale → translate → uncenter).
// Identical to compositor_blend.wgsl lines 36-56.
fn transform_uv(uv: vec2<f32>, desc: LayerDesc) -> vec2<f32> {
    var blend_uv = uv - vec2<f32>(0.5);
    blend_uv.x *= desc.aspect_ratio;

    let cos_r = cos(desc.rotation);
    let sin_r = sin(desc.rotation);
    blend_uv = vec2<f32>(
        blend_uv.x * cos_r - blend_uv.y * sin_r,
        blend_uv.x * sin_r + blend_uv.y * cos_r,
    );

    blend_uv.x /= desc.aspect_ratio;

    let s_val = max(desc.scale_val, 0.01);
    blend_uv /= s_val;

    blend_uv -= vec2<f32>(desc.translate_x, desc.translate_y);
    blend_uv += vec2<f32>(0.5);
    return blend_uv;
}

// Apply a single blend operation. Returns the composited result.
// Identical to compositor_blend.wgsl blend mode logic (all 13 modes).
fn apply_blend(base: vec4<f32>, blend: vec4<f32>, desc: LayerDesc) -> vec4<f32> {
    let ba = base.a;
    let bl_a = blend.a;
    let b = base.rgb;

    // Unpremultiply blend for non-Normal/Stencil/Opaque blends
    var f_val = blend.rgb;
    if desc.blend_mode != 0u && desc.blend_mode != 5u && desc.blend_mode != 6u {
        if blend.a > 0.001 {
            f_val = blend.rgb / max(blend.a, 0.01);
        } else {
            f_val = vec3<f32>(0.0);
        }
    }

    var blended: vec3<f32>;

    switch desc.blend_mode {
        // 0: Normal — premultiplied alpha-over
        case 0u: {
            let out_a = bl_a + ba * (1.0 - bl_a);
            var out_rgb = f_val + b * (1.0 - bl_a);
            if desc.invert_colors > 0.5 {
                out_rgb = max(vec3<f32>(1.0) - out_rgb, vec3<f32>(0.0));
            }
            let result = vec4<f32>(out_rgb, out_a);
            return mix(base, result, desc.opacity);
        }
        // 1: Additive
        case 1u: { blended = b + f_val; }
        // 2: Multiply
        case 2u: { blended = b * f_val; }
        // 3: Screen — HDR-safe
        case 3u: {
            let sb = clamp(b, vec3<f32>(0.0), vec3<f32>(1.0));
            let sf = clamp(f_val, vec3<f32>(0.0), vec3<f32>(1.0));
            blended = vec3<f32>(1.0) - (vec3<f32>(1.0) - sb) * (vec3<f32>(1.0) - sf)
                    + max(vec3<f32>(0.0), b - vec3<f32>(1.0))
                    + max(vec3<f32>(0.0), f_val - vec3<f32>(1.0));
        }
        // 4: Overlay — HDR-safe
        case 4u: {
            let sb = clamp(b, vec3<f32>(0.0), vec3<f32>(1.0));
            let sf = clamp(f_val, vec3<f32>(0.0), vec3<f32>(1.0));
            let lo = 2.0 * sb * sf;
            let hi = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - sb) * (vec3<f32>(1.0) - sf);
            blended = select(hi, lo, sb < vec3<f32>(0.5))
                    + max(vec3<f32>(0.0), b - vec3<f32>(1.0))
                    + max(vec3<f32>(0.0), f_val - vec3<f32>(1.0));
        }
        // 5: Stencil — blend alpha masks base
        case 5u: {
            let stencil_rgb = b * bl_a;
            let stencil_a = ba * bl_a;
            let stencil_result = vec4<f32>(stencil_rgb, stencil_a);
            return mix(base, stencil_result, desc.opacity);
        }
        // 6: Opaque — fully replace, ignore alpha
        case 6u: {
            return vec4<f32>(mix(b, f_val, desc.opacity), 1.0);
        }
        // 7: Difference
        case 7u: { blended = abs(b - f_val); }
        // 8: Exclusion
        case 8u: { blended = max(vec3<f32>(0.0), b + f_val - 2.0 * b * f_val); }
        // 9: Subtract
        case 9u: { blended = max(b - f_val, vec3<f32>(0.0)); }
        // 10: ColorDodge — unclamped HDR, cap at 100.0
        case 10u: {
            blended = vec3<f32>(
                select(b.r / (1.0 - f_val.r), 100.0, f_val.r >= 0.999),
                select(b.g / (1.0 - f_val.g), 100.0, f_val.g >= 0.999),
                select(b.b / (1.0 - f_val.b), 100.0, f_val.b >= 0.999),
            );
        }
        // 11: Lighten
        case 11u: { blended = max(b, f_val); }
        // 12: Darken
        case 12u: { blended = min(b, f_val); }
        default: { blended = f_val; }
    }

    // Alpha compositing: lerp base → blended by blend alpha
    var out_rgb = mix(b, blended, bl_a);
    let out_a = bl_a + ba * (1.0 - bl_a);

    // Post-blend invert
    if desc.invert_colors > 0.5 {
        out_rgb = max(vec3<f32>(1.0) - out_rgb, vec3<f32>(0.0));
    }

    // Post-blend opacity lerp
    let blended_result = vec4<f32>(out_rgb, out_a);
    return mix(base, blended_result, desc.opacity);
}

@compute @workgroup_size(16, 16, 1)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.width || id.y >= params.height {
        return;
    }

    let uv = (vec2<f32>(f32(id.x), f32(id.y)) + 0.5) / vec2<f32>(f32(params.width), f32(params.height));

    // Start from base accumulation (layers composited before this batch)
    var result = textureSampleLevel(base_tex, tex_sampler, uv, 0.0);

    // Blend each layer in sequence
    for (var i = 0u; i < params.layer_count; i++) {
        let desc = layers[i];

        // Transform UV for this layer
        let layer_uv = transform_uv(uv, desc);

        // Out-of-bounds → skip (transparent)
        if layer_uv.x < 0.0 || layer_uv.x > 1.0 || layer_uv.y < 0.0 || layer_uv.y > 1.0 {
            continue;
        }

        // Sample layer texture from binding array
        let layer_color = textureSampleLevel(
            layer_textures[desc.texture_index], tex_sampler, layer_uv, 0.0
        );

        result = apply_blend(result, layer_color, desc);
    }

    textureStore(output_tex, vec2<i32>(id.xy), result);
}
