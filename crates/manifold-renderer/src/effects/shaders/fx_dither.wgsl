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

// Bayer 8x8 ordered dither — explicit lookup table matching Unity DitherEffect.shader
fn bayer_threshold(pixel_pos: vec2<f32>) -> f32 {
    let px = i32(abs(pixel_pos.x)) % 8;
    let py = i32(abs(pixel_pos.y)) % 8;
    let idx = py * 8 + px;
    // Standard Bayer 8x8 matrix (Unity: DitherEffect.shader lines 56-65)
    // Row-major: bayer8x8[y*8+x] / 64.0
    var bayer = array<f32, 64>(
         0.0/64.0, 32.0/64.0,  8.0/64.0, 40.0/64.0,  2.0/64.0, 34.0/64.0, 10.0/64.0, 42.0/64.0,
        48.0/64.0, 16.0/64.0, 56.0/64.0, 24.0/64.0, 50.0/64.0, 18.0/64.0, 58.0/64.0, 26.0/64.0,
        12.0/64.0, 44.0/64.0,  4.0/64.0, 36.0/64.0, 14.0/64.0, 46.0/64.0,  6.0/64.0, 38.0/64.0,
        60.0/64.0, 28.0/64.0, 52.0/64.0, 20.0/64.0, 62.0/64.0, 30.0/64.0, 54.0/64.0, 22.0/64.0,
         3.0/64.0, 35.0/64.0, 11.0/64.0, 43.0/64.0,  1.0/64.0, 33.0/64.0,  9.0/64.0, 41.0/64.0,
        51.0/64.0, 19.0/64.0, 59.0/64.0, 27.0/64.0, 49.0/64.0, 17.0/64.0, 57.0/64.0, 25.0/64.0,
        15.0/64.0, 47.0/64.0,  7.0/64.0, 39.0/64.0, 13.0/64.0, 45.0/64.0,  5.0/64.0, 37.0/64.0,
        63.0/64.0, 31.0/64.0, 55.0/64.0, 23.0/64.0, 61.0/64.0, 29.0/64.0, 53.0/64.0, 21.0/64.0
    );
    return bayer[idx];
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
