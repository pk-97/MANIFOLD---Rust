// Compute variant of fluid_display.wgsl.
// Identical math — only I/O mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
    color_mode: f32,
    color_bright: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;
@group(0) @binding(2) var s_density: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Scale UV around center: >1 zooms in, <1 zooms out (tiles)
    // Unity: float2 uv = (i.uv - 0.5) / max(_UVScale, 0.001) + 0.5
    let uv = (uv_raw - vec2<f32>(0.5)) / max(params.uv_scale, 0.001) + vec2<f32>(0.5);

    // Unified texture: .r = density, .gba = pre-normalized hue
    let tex = textureSampleLevel(t_density, s_density, uv, 0.0);
    let density = tex.r;

    // Extended Reinhard tone curve: x*(1 + x/W^2) / (1 + x), W = 3.0
    // Unity: float x = density * _Intensity * _Contrast; lum = x*(1+x/9) / (1+x)
    let x = density * params.intensity * params.contrast;
    var lum = x * (1.0 + x / 9.0) / (1.0 + x);

    if params.color_mode > 0.5 {
        // --- Color path ---
        // Hue is pre-normalized in resolve: .gba = rgb/energy, (1,1,1) when no data
        let hue = tex.gba;

        // Blend between white and the hue based on Color Bright.
        // 0 = fully white (mono), 1 = balanced, >1 = saturated color
        // Unity: rgb = lerp(float3(1,1,1), hue, saturate(_ColorBright))
        var rgb = mix(vec3<f32>(1.0), hue, clamp(params.color_bright, 0.0, 1.0));

        // Apply brightness from scalar density (same curve as mono)
        rgb *= lum;

        if params.invert > 0.5 {
            rgb = 1.0 - rgb;
        }

        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(rgb, 1.0));
    } else {
        // --- Mono path ---
        if params.invert > 0.5 {
            lum = 1.0 - lum;
        }

        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(lum, lum, lum, lum));
    }
}
