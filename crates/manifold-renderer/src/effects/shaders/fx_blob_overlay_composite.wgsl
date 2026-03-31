// Composites blob tracking overlay onto source.
// overlay is premultiplied-alpha: rgb = color * alpha, a = alpha.
// Adds overlay with amount control + subtle scanline effect.

struct Uniforms {
    amount: f32,
    resolution_y: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var t_source: texture_2d<f32>;
@group(0) @binding(2) var t_overlay: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var t_output: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(t_source);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let src = textureSampleLevel(t_source, samp, uv, 0.0);
    let overlay = textureSampleLevel(t_overlay, samp, uv, 0.0);

    // Scanline
    let scanline = abs(fract(uv.y * u.resolution_y * 0.5) - 0.5) * 2.0;
    let scan_alpha = (1.0 - smoothstep(0.4, 0.5, scanline)) * 0.04;
    let scan_contrib = vec3<f32>(0.85, 0.92, 1.0) * scan_alpha;

    // Composite: additive overlay + scanline, mixed by amount
    let combined = src.rgb + (overlay.rgb + scan_contrib) * u.amount;
    textureStore(t_output, vec2<i32>(id.xy), vec4<f32>(combined, src.a));
}
