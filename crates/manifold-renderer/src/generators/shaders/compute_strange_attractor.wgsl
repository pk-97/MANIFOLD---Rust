// Display shader for ComputeStrangeAttractor.
// Reads scatter density texture, applies extended Reinhard tone mapping.
// Port of Unity GeneratorFluidParticleDisplay.shader (mono path only).
//
// Extended Reinhard: lum = x * (1 + x/9) / (1 + x)  where x = density * intensity * contrast

struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;
@group(0) @binding(2) var s_density: sampler;

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
    // UV scale: >1 zooms in — Unity: uv = (i.uv - 0.5) / max(_UVScale, 0.001) + 0.5
    let uv = (in.uv - 0.5) / max(params.uv_scale, 0.001) + 0.5;

    let density = textureSample(t_density, s_density, uv).r;

    // Extended Reinhard: x * (1 + x/9) / (1 + x)
    let x   = density * params.intensity * params.contrast;
    var lum = x * (1.0 + x / 9.0) / (1.0 + x);

    if params.invert > 0.5 {
        lum = 1.0 - lum;
    }

    lum = clamp(lum, 0.0, 1.0);
    return vec4<f32>(lum, lum, lum, lum);
}
