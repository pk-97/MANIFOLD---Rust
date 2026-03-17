// FluidParticleDisplay — port of Unity GeneratorFluidParticleDisplay.shader
// Extended Reinhard tone mapping of density field with 2 display modes:
//   Mono:  lum = extended Reinhard; output = vec4(lum, lum, lum, lum)
//   Color: scalar density drives brightness; color texture provides hue only.
//
// WHITE_POINT = 3.0 (matches Unity #define WHITE_POINT 3.0)
// UV scale: (uv - 0.5) / max(uv_scale, 0.001) + 0.5  (>1 zooms in, <1 zooms out)

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
@group(0) @binding(3) var t_color: texture_2d<f32>;
@group(0) @binding(4) var s_color: sampler;

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
    // Scale UV around center: >1 zooms in, <1 zooms out (tiles)
    // Unity: float2 uv = (i.uv - 0.5) / max(_UVScale, 0.001) + 0.5
    let uv = (in.uv - vec2<f32>(0.5)) / max(params.uv_scale, 0.001) + vec2<f32>(0.5);

    // Scalar density drives brightness for BOTH paths (identical contrast)
    let density = textureSample(t_density, s_density, uv).r;

    // Extended Reinhard tone curve: x*(1 + x/W^2) / (1 + x), W = 3.0
    // Unity: float x = density * _Intensity * _Contrast; lum = x*(1+x/9) / (1+x)
    let x = density * params.intensity * params.contrast;
    var lum = x * (1.0 + x / 9.0) / (1.0 + x);

    if params.color_mode > 0.5 {
        // --- Color path ---
        // Use scalar density for brightness (matches mono exactly).
        // Color texture provides hue only (pre-normalized in ResolveColorKernel).
        let col = textureSample(t_color, s_color, uv);

        // Hue is pre-normalized: if a > 0.001 use col.rgb, else white
        // Unity: float3 hue = col.a > 0.001 ? col.rgb : float3(1,1,1)
        let hue = select(vec3<f32>(1.0), col.rgb, col.a > 0.001);

        // Blend between white and the hue based on Color Bright.
        // 0 = fully white (mono), 1 = balanced, >1 = saturated color
        // Unity: rgb = lerp(float3(1,1,1), hue, saturate(_ColorBright))
        var rgb = mix(vec3<f32>(1.0), hue, clamp(params.color_bright, 0.0, 1.0));

        // Apply brightness from scalar density (same curve as mono)
        rgb *= lum;

        if params.invert > 0.5 {
            rgb = 1.0 - rgb;
        }

        return vec4<f32>(rgb, 1.0);
    } else {
        // --- Mono path ---
        if params.invert > 0.5 {
            lum = 1.0 - lum;
        }

        return vec4<f32>(lum, lum, lum, lum);
    }
}
