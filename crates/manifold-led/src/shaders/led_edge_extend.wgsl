// LED edge-extend shader with spatial blur.
// Samples left/right edge bands from the compositor texture into a tiny
// pixel grid (strips × LEDs). Each sample is a box-averaged region of the
// source, eliminating single-pixel flicker on physical LEDs.
// Unity equivalent: LEDEdgeExtend.shader (enhanced with blur)

struct Uniforms {
    left_edge_width: f32,
    right_edge_width: f32,
    // Blur radius in source texels. 0 = no blur (single sample).
    blur_radius: f32,
    _pad: f32,
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
    // Compute source U coordinate from edge-extend mapping
    var source_u: f32;
    if in.uv.x < 0.5 {
        source_u = (in.uv.x / 0.5) * uniforms.left_edge_width;
    } else {
        source_u = (1.0 - uniforms.right_edge_width)
            + ((in.uv.x - 0.5) / 0.5) * uniforms.right_edge_width;
    }

    let center = vec2<f32>(source_u, in.uv.y);

    // No blur: single sample (fast path)
    if uniforms.blur_radius <= 0.0 {
        return textureSample(source_tex, tex_sampler, center);
    }

    // Box blur: sample a grid around the center point.
    // Texel size derived from source texture dimensions.
    let tex_size = vec2<f32>(textureDimensions(source_tex, 0));
    let texel = 1.0 / tex_size;
    let radius = uniforms.blur_radius;

    // 5-tap cross pattern: center + 4 cardinal offsets at full radius.
    // Cheaper than a full grid, good enough for LED smoothing.
    let offset_h = vec2<f32>(radius * texel.x, 0.0);
    let offset_v = vec2<f32>(0.0, radius * texel.y);

    var color = textureSample(source_tex, tex_sampler, center) * 2.0;
    color += textureSample(source_tex, tex_sampler, center - offset_h);
    color += textureSample(source_tex, tex_sampler, center + offset_h);
    color += textureSample(source_tex, tex_sampler, center - offset_v);
    color += textureSample(source_tex, tex_sampler, center + offset_v);
    // Half-radius diagonals for wider coverage
    let diag = vec2<f32>(radius * 0.7 * texel.x, radius * 0.7 * texel.y);
    color += textureSample(source_tex, tex_sampler, center + vec2<f32>(-diag.x, -diag.y));
    color += textureSample(source_tex, tex_sampler, center + vec2<f32>( diag.x, -diag.y));
    color += textureSample(source_tex, tex_sampler, center + vec2<f32>(-diag.x,  diag.y));
    color += textureSample(source_tex, tex_sampler, center + vec2<f32>( diag.x,  diag.y));

    return color / 10.0;
}
