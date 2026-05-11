// primitive.dither_pattern — pixel-exact replacement for legacy
// `effects/shaders/fx_dither.wgsl`. 6 dithering algorithms with
// luminance-preserving quantization. Bindings, math, and dispatch
// shape preserved verbatim.

struct Uniforms {
    amount: f32,
    algorithm: u32,
    resolution_x: f32,
    resolution_y: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

fn bayer_threshold(pixel_pos: vec2<f32>) -> f32 {
    let px = i32(abs(pixel_pos.x)) % 8;
    let py = i32(abs(pixel_pos.y)) % 8;
    let idx = py * 8 + px;
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

fn halftone_threshold(pixel_pos: vec2<f32>) -> f32 {
    let cell_size = 6.0;
    let cell = (pixel_pos % cell_size) / cell_size - 0.5;
    return clamp(length(cell) * 2.0, 0.0, 1.0);
}

fn lines_threshold(pixel_pos: vec2<f32>) -> f32 {
    let line_width = 4.0;
    return abs(fract(pixel_pos.y / line_width) - 0.5) * 2.0;
}

fn crosshatch_threshold(pixel_pos: vec2<f32>) -> f32 {
    let spacing = 5.0;
    let d1 = abs(fract((pixel_pos.x + pixel_pos.y) / spacing) - 0.5) * 2.0;
    let d2 = abs(fract((pixel_pos.x - pixel_pos.y) / spacing) - 0.5) * 2.0;
    return min(d1, d2);
}

fn noise_threshold(pixel_pos: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * pixel_pos.x + 0.00583715 * pixel_pos.y));
}

fn diamond_threshold(pixel_pos: vec2<f32>) -> f32 {
    let p = (pixel_pos % 4.0) - 2.0;
    return clamp((abs(p.x) + abs(p.y)) / 4.0, 0.0, 1.0);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let original = src.rgb;

    let pixel_pos = uv * vec2<f32>(uniforms.resolution_x, uniforms.resolution_y);

    let lum = dot(original, vec3<f32>(0.2126, 0.7152, 0.0722));

    let levels = mix(8.0, 2.0, uniforms.amount);

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

    var dithered = (lum + (threshold - 0.5) / levels) * levels;
    dithered = floor(dithered + 0.5) / levels;
    dithered = clamp(dithered, 0.0, 1.0);

    var scale: f32;
    if lum > 0.001 {
        scale = dithered / lum;
    } else {
        scale = dithered;
    }
    let dithered_color = original * scale;

    let result = mix(original, dithered_color, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, src.a));
}
