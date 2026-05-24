// node.cook_torrance_specular — physically-based microfacet specular
// from a tangent-space normal map + directional light + view. ADDITIVE.
//
// Bindings:
//   @binding(0) uniforms (96 bytes — see CookTorranceUniforms in Rust)
//   @binding(1) tex_normal
//   @binding(2) tex_sampler
//   @binding(3) output_tex (rgba16float storage)
//
// Prepended at pipeline-creation: pbr_brdf.wgsl (pbr_cook_torrance_specular,
// pbr_f_schlick, PBR_PI).

struct Uniforms {
    light: vec3<f32>,
    roughness: f32,
    view: vec3<f32>,
    metallic: f32,
    light_color: vec4<f32>, // rgb = color, a = intensity
    base_color: vec4<f32>,
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
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let N = normalize(textureSampleLevel(tex_normal, tex_sampler, uv, 0.0).rgb + vec3<f32>(1e-8));
    let L = normalize(uniforms.light + vec3<f32>(1e-8));
    let V = normalize(uniforms.view + vec3<f32>(1e-8));

    let F0 = mix(vec3<f32>(0.04), uniforms.base_color.rgb, uniforms.metallic);
    let NdotL = max(dot(N, L), 0.0);

    let spec = pbr_cook_torrance_specular(N, L, V, max(uniforms.roughness, 0.01), F0);
    let intensity = uniforms.light_color.a;
    let direct = spec * uniforms.light_color.rgb * intensity * NdotL;

    let scalar = max(max(direct.x, direct.y), direct.z);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(direct, scalar));
}
