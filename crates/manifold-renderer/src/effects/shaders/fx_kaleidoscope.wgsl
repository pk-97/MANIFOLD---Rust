// Kaleidoscope effect — polar-coordinate segment mirroring.

struct Uniforms {
    amount: f32,
    segments: f32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

const TAU: f32 = 6.28318530718;

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

    // Center UV at origin
    let centered = in.uv - 0.5;

    // Convert to polar coordinates
    let angle = atan2(centered.y, centered.x);
    let radius = length(centered);

    // Slice angle into segments and mirror alternating slices
    let segment_angle = TAU / uniforms.segments;
    let slice_index = floor(angle / segment_angle);
    var local_angle = angle - slice_index * segment_angle;

    // Mirror odd slices for seamless reflection
    if (abs(slice_index) % 2.0) > 0.5 {
        local_angle = segment_angle - local_angle;
    }

    // Convert back to cartesian
    var kaleid_uv: vec2<f32>;
    kaleid_uv.x = cos(local_angle) * radius + 0.5;
    kaleid_uv.y = sin(local_angle) * radius + 0.5;

    // Clamp to valid UV range
    kaleid_uv = clamp(kaleid_uv, vec2<f32>(0.0), vec2<f32>(1.0));

    let kaleid_sample = textureSample(source_tex, tex_sampler, kaleid_uv);
    let result = mix(original, kaleid_sample.rgb, uniforms.amount);
    return vec4<f32>(result, mix(src.a, kaleid_sample.a, uniforms.amount));
}
