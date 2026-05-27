// node.cook_torrance_specular — physically-based microfacet specular
// from a normal map + light + view. ADDITIVE specular term only.
//
// Two modes (picked by `use_world_pos`):
//   0 = flat screen-space: light/view in uniforms are unit DIRECTIONS
//       constant across the image. No attenuation. Normal is tangent-space.
//   1 = 3D mesh: light/view are world POSITIONS. Per-pixel V and L come
//       from `world_pos` texture (Rgba16Float, .xyz = world coords,
//       .a >= 0.5 where geometry covers). Inverse-square attenuation
//       1 / (1 + d²/attenuation_scale) applied. Normal is world-space.
//
// Bindings:
//   @binding(0) uniforms (CookTorranceUniforms)
//   @binding(1) tex_normal           — normal map (tangent or world space)
//   @binding(2) tex_sampler
//   @binding(3) output_tex           — Rgba16Float storage write
//   @binding(4) tex_world_pos        — Rgba16Float; ignored when use_world_pos=0
//   @binding(5) tex_roughness_map    — sampled .r supersedes scalar roughness
//                                       when use_roughness_map=1
//
// Prepended at pipeline-creation: pbr_brdf.wgsl
// (pbr_cook_torrance_specular, pbr_f_schlick, PBR_PI).

struct Uniforms {
    light: vec3<f32>,
    roughness: f32,
    view: vec3<f32>,
    metallic: f32,
    light_color: vec4<f32>, // rgb = color, a = intensity
    base_color: vec4<f32>,
    use_world_pos: u32,
    use_roughness_map: u32,
    attenuation_scale: f32,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_normal: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var tex_world_pos: texture_2d<f32>;
@group(0) @binding(5) var tex_roughness_map: texture_2d<f32>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let N = normalize(textureSampleLevel(tex_normal, tex_sampler, uv, 0.0).rgb + vec3<f32>(1e-8));

    var V: vec3<f32>;
    var L: vec3<f32>;
    var attenuation: f32;

    if uniforms.use_world_pos == 1u {
        let wp_sample = textureSampleLevel(tex_world_pos, tex_sampler, uv, 0.0);
        if wp_sample.a < 0.5 {
            // Background pixel — no geometry, no shading.
            textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(0.0));
            return;
        }
        let world_pos = wp_sample.xyz;
        V = normalize(uniforms.view - world_pos + vec3<f32>(1e-8));
        L = normalize(uniforms.light - world_pos + vec3<f32>(1e-8));
        let light_dist = length(uniforms.light - world_pos);
        attenuation = 1.0 / (1.0 + light_dist * light_dist / uniforms.attenuation_scale);
    } else {
        L = normalize(uniforms.light + vec3<f32>(1e-8));
        V = normalize(uniforms.view + vec3<f32>(1e-8));
        attenuation = 1.0;
    }

    let F0 = mix(vec3<f32>(0.04), uniforms.base_color.rgb, uniforms.metallic);
    let NdotL = max(dot(N, L), 0.0);

    var roughness: f32 = uniforms.roughness;
    if uniforms.use_roughness_map == 1u {
        roughness = textureSampleLevel(tex_roughness_map, tex_sampler, uv, 0.0).r;
    }

    let spec = pbr_cook_torrance_specular(N, L, V, max(roughness, 0.01), F0);
    let intensity = uniforms.light_color.a;
    let direct = spec * uniforms.light_color.rgb * intensity * NdotL * attenuation;

    let scalar = max(max(direct.x, direct.y), direct.z);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(direct, scalar));
}
