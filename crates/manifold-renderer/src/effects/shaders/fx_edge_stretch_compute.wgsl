// Compute variant of fx_edge_stretch.wgsl — same math, no TBDR tile overhead.
// EdgeStretch effect — clamps UVs to a center strip, stretching edge pixels.

struct Uniforms {
    amount: f32,
    source_width: f32,  // 0.1..0.9 — width of the visible center strip
    mode: u32,          // 0=Horizontal, 1=Vertical, 2=Both
    _pad: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(source_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }

    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let src = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let original = src.rgb;

    let half_width = uniforms.source_width * 0.5;
    let left_edge = 0.5 - half_width;
    let right_edge = 0.5 + half_width;

    var stretch_uv = uv;

    // 0 = Horizontal, 1 = Vertical, 2 = Both
    if uniforms.mode == 0u || uniforms.mode == 2u {
        stretch_uv.x = clamp(stretch_uv.x, left_edge, right_edge);
    }
    if uniforms.mode == 1u || uniforms.mode == 2u {
        stretch_uv.y = clamp(stretch_uv.y, left_edge, right_edge);
    }

    let stretch_sample = textureSampleLevel(source_tex, tex_sampler, stretch_uv, 0.0);
    let result = mix(original, stretch_sample.rgb, uniforms.amount);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(result, mix(src.a, stretch_sample.a, uniforms.amount)));
}
