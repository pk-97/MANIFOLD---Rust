// node.brightness — RGB → weighted grayscale (luma) via per-channel
// weights. Defaults are BT.709 luma coefficients, so the default behaviour
// is desaturate-to-luminance. The `luma_for_height` / `luma_for_sobel`
// pattern in MetallicGlass: collapse a colour field to a scalar before a
// heightmap or edge-detection pass.

struct Uniforms {
    weights: vec4<f32>, // xyz = per-channel weights; w unused (padding)
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
    let g = dot(src.rgb, uniforms.weights.xyz);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(g, g, g, src.a));
}
