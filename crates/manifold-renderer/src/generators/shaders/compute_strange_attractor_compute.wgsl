// Compute variant of compute_strange_attractor.wgsl (display pass).
// Identical math — only I/O mechanism changes:
//   - textureSampleLevel instead of textureSample
//   - textureStore to output storage texture instead of fragment return
//   - @compute @workgroup_size(16,16) instead of vertex+fragment

struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    invert: f32,
    uv_scale: f32,
};

@group(0) @binding(0) var<uniform> params: DisplayUniforms;
@group(0) @binding(1) var t_density: texture_2d<f32>;
@group(0) @binding(2) var s_density: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if (gid.x >= u32(dims.x) || gid.y >= u32(dims.y)) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);

    // UV scale: >1 zooms in — Unity: uv = (i.uv - 0.5) / max(_UVScale, 0.001) + 0.5
    let scaled_uv = (uv - 0.5) / max(params.uv_scale, 0.001) + 0.5;

    let density = textureSampleLevel(t_density, s_density, scaled_uv, 0.0).r;

    // Extended Reinhard: x * (1 + x/9) / (1 + x)
    let x   = density * params.intensity * params.contrast;
    var lum = x * (1.0 + x / 9.0) / (1.0 + x);

    if params.invert > 0.5 {
        lum = 1.0 - lum;
    }

    lum = clamp(lum, 0.0, 1.0);
    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(lum, lum, lum, lum));
}
