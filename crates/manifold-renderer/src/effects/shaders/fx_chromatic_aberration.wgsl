// ChromaticAberration effect — radial or linear RGB channel separation.

struct Uniforms {
    amount: f32,
    mode: u32,       // 0=Radial, 1=Linear
    angle: f32,
    falloff: f32,
    offset: f32,
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

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let center = vec2<f32>(0.5, 0.5);
    let delta = in.uv - center;

    var dir: vec2<f32>;
    if uniforms.mode == 0u {
        // Radial mode: offset along direction from center
        let dist = length(delta);
        var radial_mask = smoothstep(0.0, 0.707, dist);
        radial_mask = mix(radial_mask, 1.0, 1.0 - uniforms.falloff);
        if dist > 1e-5 {
            dir = normalize(delta) * radial_mask;
        } else {
            dir = vec2<f32>(1.0, 0.0);
        }
    } else {
        // Linear mode: offset along fixed angle
        let rad = uniforms.angle * 0.01745329;
        dir = vec2<f32>(cos(rad), sin(rad));
    }

    let offset_r = dir * uniforms.offset;
    let offset_b = -dir * uniforms.offset;

    let r = textureSample(source_tex, tex_sampler, in.uv + offset_r).r;
    let g = textureSample(source_tex, tex_sampler, in.uv).g;
    let b = textureSample(source_tex, tex_sampler, in.uv + offset_b).b;
    let src = textureSample(source_tex, tex_sampler, in.uv);

    let effected = vec3<f32>(r, g, b);
    let result = mix(src.rgb, effected, uniforms.amount);
    return vec4<f32>(result, src.a);
}
