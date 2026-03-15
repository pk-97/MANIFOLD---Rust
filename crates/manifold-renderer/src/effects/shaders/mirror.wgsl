// Mirror effect — horizontal, vertical, or quad mirror.

struct Uniforms {
    mode: u32,  // 0=horizontal, 1=vertical, 2=quad
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
    var uv = in.uv;

    if uniforms.mode == 0u {
        // Horizontal mirror: reflect left half to right
        uv.x = select(uv.x, 1.0 - uv.x, uv.x > 0.5);
    } else if uniforms.mode == 1u {
        // Vertical mirror: reflect top half to bottom
        uv.y = select(uv.y, 1.0 - uv.y, uv.y > 0.5);
    } else {
        // Quad mirror: reflect both axes
        uv.x = select(uv.x, 1.0 - uv.x, uv.x > 0.5);
        uv.y = select(uv.y, 1.0 - uv.y, uv.y > 0.5);
    }

    return textureSample(source_tex, tex_sampler, uv);
}
