// Strange attractor display — extended Reinhard tone mapping with invert toggle.
// Based on fluid_display_compute.wgsl, adds invert parameter for attractors.

struct DisplayUniforms {
    intensity: f32,
    contrast: f32,
    uv_scale: f32,
    invert: f32,
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

    let uv_raw = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let uv = (uv_raw - vec2<f32>(0.5)) / max(params.uv_scale, 0.001) + vec2<f32>(0.5);

    let density = textureSampleLevel(t_density, s_density, uv, 0.0).r;

    // Extended Reinhard tone curve: x*(1 + x/W^2) / (1 + x), W = 3.0
    let x = density * params.intensity * params.contrast;
    var lum = x * (1.0 + x / 9.0) / (1.0 + x);

    // Optional invert
    lum = mix(lum, 1.0 - lum, step(0.5, params.invert));

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(lum, lum, lum, lum));
}
