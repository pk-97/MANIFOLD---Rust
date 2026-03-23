// MRI Slice Viewer — 2D texture mode
// Displays a pre-sliced TIFF loaded as an R8Unorm 2D texture.
// Applies window/level, unsharp mask sharpening, and tone curve.

struct Uniforms {
    aspect_ratio: f32,
    uv_scale: f32,
    invert: f32,
    sharpen: f32,
    window_center: f32,
    window_width: f32,
    tex_width: f32,
    tex_height: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var slice_tex: texture_2d<f32>;
@group(0) @binding(2) var slice_sampler: sampler;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(vi) / 2) * 4.0 - 1.0;
    let y = f32(i32(vi) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Fit image to output preserving native aspect ratio
    let img_aspect = u.tex_width / u.tex_height;
    let ratio = u.aspect_ratio / img_aspect;
    var uv = in.uv - 0.5;
    if ratio > 1.0 {
        // Output wider than image — pillarbox
        uv.x *= ratio;
    } else {
        // Output narrower than image — letterbox
        uv.y /= ratio;
    }
    // User zoom
    uv = uv * u.uv_scale + 0.5;

    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let center = textureSampleLevel(slice_tex, slice_sampler, uv, 0.0).r;

    // Unsharp mask: 4-neighbor Laplacian sharpening
    var sharpened = center;
    if u.sharpen > 0.0 {
        let dx = vec2<f32>(1.0 / u.tex_width, 1.0 / u.tex_height);

        let s_l = textureSampleLevel(slice_tex, slice_sampler, uv + vec2<f32>(-dx.x, 0.0), 0.0).r;
        let s_r = textureSampleLevel(slice_tex, slice_sampler, uv + vec2<f32>( dx.x, 0.0), 0.0).r;
        let s_u = textureSampleLevel(slice_tex, slice_sampler, uv + vec2<f32>(0.0, -dx.y), 0.0).r;
        let s_d = textureSampleLevel(slice_tex, slice_sampler, uv + vec2<f32>(0.0,  dx.y), 0.0).r;

        let laplacian = 4.0 * center - (s_l + s_r + s_u + s_d);
        sharpened = center + laplacian * u.sharpen * 0.5;
    }

    // Window/level tone mapping
    let w_low = u.window_center - u.window_width * 0.5;
    let w_high = u.window_center + u.window_width * 0.5;
    var lum = clamp((sharpened - w_low) / max(w_high - w_low, 0.001), 0.0, 1.0);

    // S-curve contrast
    lum = lum * lum * (3.0 - 2.0 * lum);

    lum = mix(lum, 1.0 - lum, u.invert);

    return vec4<f32>(lum, lum, lum, 1.0);
}
