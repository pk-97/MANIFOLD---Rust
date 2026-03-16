// Dither effect — 6 algorithms: Bayer, Halftone, Lines, CrossHatch, Noise, Diamond.

struct Uniforms {
    amount: f32,
    algorithm: u32,    // 0=Bayer,1=Halftone,2=Lines,3=CrossHatch,4=Noise,5=Diamond
    resolution_x: f32,
    resolution_y: f32,
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

// Bayer 8x8 — computed procedurally to avoid array indexing issues
fn bayer_threshold(pixel_pos: vec2<f32>) -> f32 {
    let p = vec2<i32>(i32(pixel_pos.x) % 8, i32(pixel_pos.y) % 8);
    // Compute bayer value using bit-reversal interleave
    var val = 0;
    var x = p.x;
    var y = p.y;
    // 3-bit interleave
    val = val | ((x & 1) << 0) | ((y & 1) << 1);
    val = val | (((x >> 1) & 1) << 2) | (((y >> 1) & 1) << 3);
    val = val | (((x >> 2) & 1) << 4) | (((y >> 2) & 1) << 5);
    // Reverse bits for proper Bayer pattern
    var r = 0;
    r = r | ((val >> 5) & 1) << 0;
    r = r | ((val >> 4) & 1) << 1;
    r = r | ((val >> 3) & 1) << 2;
    r = r | ((val >> 2) & 1) << 3;
    r = r | ((val >> 1) & 1) << 4;
    r = r | ((val >> 0) & 1) << 5;
    return f32(r) / 64.0;
}

// Halftone dots
fn halftone_threshold(pixel_pos: vec2<f32>) -> f32 {
    let cell_size = 6.0;
    let cell = (pixel_pos % cell_size) / cell_size - 0.5;
    return clamp(length(cell) * 2.0, 0.0, 1.0);
}

// Horizontal scanlines
fn lines_threshold(pixel_pos: vec2<f32>) -> f32 {
    let line_width = 4.0;
    return abs(fract(pixel_pos.y / line_width) - 0.5) * 2.0;
}

// Crosshatch (45 + 135 degrees)
fn crosshatch_threshold(pixel_pos: vec2<f32>) -> f32 {
    let spacing = 5.0;
    let d1 = abs(fract((pixel_pos.x + pixel_pos.y) / spacing) - 0.5) * 2.0;
    let d2 = abs(fract((pixel_pos.x - pixel_pos.y) / spacing) - 0.5) * 2.0;
    return min(d1, d2);
}

// Blue noise (interleaved gradient noise, Jimenez 2014)
fn noise_threshold(pixel_pos: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * pixel_pos.x + 0.00583715 * pixel_pos.y));
}

// Diamond 4x4 ordered dither
fn diamond_threshold(pixel_pos: vec2<f32>) -> f32 {
    let p = (pixel_pos % 4.0) - 2.0;
    return clamp((abs(p.x) + abs(p.y)) / 4.0, 0.0, 1.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = textureSample(source_tex, tex_sampler, in.uv);
    let original = src.rgb;

    // Pixel position in texels
    let pixel_pos = in.uv * vec2<f32>(uniforms.resolution_x, uniforms.resolution_y);

    // Luminance (Rec.709)
    let lum = dot(original, vec3<f32>(0.2126, 0.7152, 0.0722));

    // Quantization levels: 8 (subtle) -> 2 (extreme) as Amount goes 0 -> 1
    let levels = mix(8.0, 2.0, uniforms.amount);

    // Select dither threshold based on algorithm
    var threshold: f32;
    if uniforms.algorithm == 0u {
        threshold = bayer_threshold(pixel_pos);
    } else if uniforms.algorithm == 1u {
        threshold = halftone_threshold(pixel_pos);
    } else if uniforms.algorithm == 2u {
        threshold = lines_threshold(pixel_pos);
    } else if uniforms.algorithm == 3u {
        threshold = crosshatch_threshold(pixel_pos);
    } else if uniforms.algorithm == 4u {
        threshold = noise_threshold(pixel_pos);
    } else {
        threshold = diamond_threshold(pixel_pos);
    }

    // Dither-biased quantization
    var dithered = (lum + (threshold - 0.5) / levels) * levels;
    dithered = floor(dithered + 0.5) / levels;
    dithered = clamp(dithered, 0.0, 1.0);

    // Preserve hue: scale original color by luminance ratio
    var scale: f32;
    if lum > 0.001 {
        scale = dithered / lum;
    } else {
        scale = dithered;
    }
    let dithered_color = original * scale;

    // Crossfade with original
    let result = mix(original, dithered_color, uniforms.amount);
    return vec4<f32>(result, src.a);
}
