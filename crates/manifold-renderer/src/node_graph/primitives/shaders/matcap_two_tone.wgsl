// node.matcap_two_tone — cross-axis 4-colour matcap from a
// tangent-space normal map.
//
// per-pixel:
//   mc_uv = n.xy * 0.5 + 0.5
//   base  = mix(color_y_low, color_y_high, clamp(mc_uv.y, 0, 1))
//   side  = mix(color_x_low, color_x_high, clamp(mc_uv.x, 0, 1))
//   out.rgb = (base + side) * 0.5;  out.a = 1.0
//
// The "stylised PBR base" atom: two 2-tone gradients summed by axis.
// Pair upstream with `node.heightmap_to_normal` and downstream with
// `node.fresnel_rim` + `node.blinn_specular` (added together) for the
// full oily-fluid PBR look. Drop in a single instance for a clean
// 4-corner matcap on any normal map.
//
// Bindings:
//   @binding(0) uniforms (64 bytes — 4 × vec4)
//   @binding(1) tex_normal
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)

struct Uniforms {
    color_y_low: vec4<f32>,
    color_y_high: vec4<f32>,
    color_x_low: vec4<f32>,
    color_x_high: vec4<f32>,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_normal: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let inv = vec2<f32>(1.0) / vec2<f32>(dims);
    let uv = (vec2<f32>(id.xy) + 0.5) * inv;

    let n = textureSampleLevel(tex_normal, tex_sampler, uv, 0.0).rgb;
    let mc = n.xy * 0.5 + 0.5;
    let base = mix(uniforms.color_y_low.rgb, uniforms.color_y_high.rgb, clamp(mc.y, 0.0, 1.0));
    let side = mix(uniforms.color_x_low.rgb, uniforms.color_x_high.rgb, clamp(mc.x, 0.0, 1.0));
    let col = (base + side) * 0.5;
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(col, 1.0));
}
