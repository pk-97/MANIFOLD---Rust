// node.reinhard_tone_map — extended Reinhard tone mapping on an
// HDR Texture2D source. Extracted from
// generators/shaders/fluid_display_compute.wgsl.
//
// Curve: x*(1 + x/W²) / (1 + x) with W = 3.0 (matches the FluidSim
// display curve exactly). Operates per-channel on the RGB source;
// alpha passes through unchanged.
//
// For SDR-only display use cases. For multi-curve / HDR-aware
// tone mapping (ACES, AgX, Khronos PBR, PQ/EDR output), use
// node.tone_map instead.

struct ReinhardUniforms {
    intensity: f32,
    contrast: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: ReinhardUniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var s_source: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if gid.x >= u32(dims.x) || gid.y >= u32(dims.y) {
        return;
    }

    let uv = (vec2<f32>(gid.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(t_source, s_source, uv, 0.0);

    let x = src.rgb * u.intensity * u.contrast;
    // Extended Reinhard with white-point W = 3.0
    let mapped = x * (1.0 + x / vec3<f32>(9.0)) / (1.0 + x);

    textureStore(output_tex, vec2<i32>(gid.xy), vec4<f32>(mapped, src.a));
}
