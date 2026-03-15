// Bloom effect — multi-purpose shader with mode selection.
// Mode 0: Prefilter (threshold + soft knee)
// Mode 1: Downsample (box filter)
// Mode 2: Upsample (tent filter + additive blend)
// Mode 3: Composite (lerp original with bloom)

struct Uniforms {
    mode: u32,
    threshold: f32,
    intensity: f32,
    texel_size_x: f32,
    texel_size_y: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    let x = f32(i32(vi & 1u)) * 4.0 - 1.0;
    let y = f32(i32(vi >> 1u)) * 4.0 - 1.0;
    var out: VertexOutput;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let ts = vec2<f32>(uniforms.texel_size_x, uniforms.texel_size_y);

    if uniforms.mode == 0u {
        // Prefilter: extract bright areas above threshold
        let color = textureSample(source_tex, tex_sampler, in.uv);
        let lum = luminance(color.rgb);
        let knee = uniforms.threshold * 0.5;
        let soft = clamp(lum - uniforms.threshold + knee, 0.0, knee * 2.0);
        let contribution = select(0.0, soft * soft / (4.0 * knee + 0.0001), knee > 0.0);
        let brightness = max(lum - uniforms.threshold, contribution);
        let scale = select(0.0, brightness / (lum + 0.0001), lum > 0.0);
        return vec4<f32>(color.rgb * scale, color.a);
    } else if uniforms.mode == 1u {
        // Downsample: 4-tap box filter
        let a = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(-ts.x, -ts.y));
        let b = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( ts.x, -ts.y));
        let c = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(-ts.x,  ts.y));
        let d = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( ts.x,  ts.y));
        return (a + b + c + d) * 0.25;
    } else if uniforms.mode == 2u {
        // Upsample: 9-tap tent filter
        let s0 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(-ts.x, -ts.y));
        let s1 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( 0.0,  -ts.y)) * 2.0;
        let s2 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( ts.x, -ts.y));
        let s3 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(-ts.x,  0.0)) * 2.0;
        let s4 = textureSample(source_tex, tex_sampler, in.uv) * 4.0;
        let s5 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( ts.x,  0.0)) * 2.0;
        let s6 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>(-ts.x,  ts.y));
        let s7 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( 0.0,   ts.y)) * 2.0;
        let s8 = textureSample(source_tex, tex_sampler, in.uv + vec2<f32>( ts.x,  ts.y));
        return (s0 + s1 + s2 + s3 + s4 + s5 + s6 + s7 + s8) / 16.0;
    } else {
        // Composite: blend bloom with original
        let original = textureSample(source_tex, tex_sampler, in.uv);
        return original; // bloom composite handled by blending in Rust code
    }
}
