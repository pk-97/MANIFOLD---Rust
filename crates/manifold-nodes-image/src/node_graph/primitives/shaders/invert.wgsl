// node.invert — RGB invert blended against source by intensity.
//
// Pixel-exact replacement for the legacy `effects/shaders/invert_colors.wgsl`.
// Binding indices, math, workgroup shape, and dispatch shape preserved
// verbatim. Changing any of this breaks the parity test.

struct Uniforms {
    intensity: f32,
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

    let color = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    let inverted = vec4<f32>(1.0 - color.r, 1.0 - color.g, 1.0 - color.b, color.a);
    textureStore(output_tex, vec2<i32>(id.xy), mix(color, inverted, uniforms.intensity));
}
