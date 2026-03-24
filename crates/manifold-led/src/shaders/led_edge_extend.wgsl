// LED edge-extend shader.
// Samples left/right edge bands from the compositor texture into a tiny
// pixel grid (strips × LEDs). Left half maps to left edge, right half to right.
// Unity equivalent: LEDEdgeExtend.shader

struct Uniforms {
    left_edge_width: f32,
    right_edge_width: f32,
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
    // Left half: sample left edge band (0 .. left_edge_width)
    // Right half: sample right edge band (1-right_edge_width .. 1)
    var source_u: f32;
    if in.uv.x < 0.5 {
        source_u = (in.uv.x / 0.5) * uniforms.left_edge_width;
    } else {
        source_u = (1.0 - uniforms.right_edge_width)
            + ((in.uv.x - 0.5) / 0.5) * uniforms.right_edge_width;
    }

    return textureSample(source_tex, tex_sampler, vec2<f32>(source_u, in.uv.y));
}
