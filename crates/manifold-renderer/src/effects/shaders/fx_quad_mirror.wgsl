// QuadMirror effect — mirrors UVs around center in both axes, crossfades with original.

struct Uniforms {
    amount: f32,
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
    let src = textureSample(source_tex, tex_sampler, in.uv);
    let original = src.rgb;

    // Mirror UVs around center in both axes
    var mirror_uv: vec2<f32>;
    mirror_uv.x = 0.5 - abs(in.uv.x - 0.5);
    mirror_uv.y = 0.5 - abs(in.uv.y - 0.5);

    // Scale back to full range (0-0.5 -> 0-1)
    mirror_uv = mirror_uv * 2.0;

    let mirror_sample = textureSample(source_tex, tex_sampler, mirror_uv);
    let mirrored = mirror_sample.rgb;

    // 0.0->0.5: mirrors fade in, original stays at 100%
    // 0.5->1.0: original fades out, mirrors stay at 100%
    let orig_alpha = clamp((1.0 - uniforms.amount) * 2.0, 0.0, 1.0);
    let mir_alpha = clamp(uniforms.amount * 2.0, 0.0, 1.0);
    let result = clamp(original * orig_alpha + mirrored * mir_alpha, vec3<f32>(0.0), vec3<f32>(1.0));
    let a = clamp(src.a * orig_alpha + mirror_sample.a * mir_alpha, 0.0, 1.0);
    return vec4<f32>(result, a);
}
