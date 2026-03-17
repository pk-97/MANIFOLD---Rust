// Mechanical port of Unity MirrorEffect.shader.
// 0=Horizontal, 1=Vertical, 2=Both. Amount blends between original and mirrored.

struct Uniforms {
    amount: f32,  // _Amount
    mode: u32,    // _Mode: 0=horizontal, 1=vertical, 2=both
    _pad0: f32,
    _pad1: f32,
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
    // MirrorEffect.shader lines 53-66
    let src = textureSample(source_tex, tex_sampler, in.uv);

    var mirror_uv = in.uv;

    // lines 59-60: if (mode == 0 || mode == 2) mirrorUv.x = 0.5 - abs(i.uv.x - 0.5);
    if uniforms.mode == 0u || uniforms.mode == 2u {
        mirror_uv.x = 0.5 - abs(in.uv.x - 0.5);
    }
    // lines 61-62: if (mode == 1 || mode == 2) mirrorUv.y = 0.5 - abs(i.uv.y - 0.5);
    if uniforms.mode == 1u || uniforms.mode == 2u {
        mirror_uv.y = 0.5 - abs(in.uv.y - 0.5);
    }

    let mirrored = textureSample(source_tex, tex_sampler, mirror_uv);

    // lines 65-66: lerp(src, mirrored, _Amount)
    let result = mix(src.rgb, mirrored.rgb, uniforms.amount);
    return vec4<f32>(result, mix(src.a, mirrored.a, uniforms.amount));
}
