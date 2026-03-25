// Compute variant of mri_slice.wgsl.
// Identical math — only I/O mechanism changes:
//   - textureSampleLevel (already used in fragment) stays the same
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

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
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // Fit image to output preserving native aspect ratio
    let img_aspect = u.tex_width / u.tex_height;
    let ratio = u.aspect_ratio / img_aspect;
    var uv = uv_raw - 0.5;
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
        textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(0.0, 0.0, 0.0, 1.0));
        return;
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

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(lum, lum, lum, 1.0));
}
