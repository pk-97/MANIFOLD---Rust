// Linear scene to display presentation. The curve implementations are shared
// with the compute tonemapper through tonemap_common.wgsl.

struct Uniforms {
    exposure: f32,
    paper_white: f32,
    max_nits: f32,
    mode: u32,  // 0 = SDR curve, 1 = EDR soft shoulder
    curve: u32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSampleLevel(t_source, s_source, in.uv, 0.0);
    let scene = src.rgb * u.exposure;
    var mapped: vec3<f32>;
    if (u.mode == 0u) {
        mapped = tonemap_sdr(scene, u.curve);
    } else {
        mapped = edr_soft_shoulder(scene, u.max_nits);
    }
    return vec4<f32>(mapped, src.a);
}
