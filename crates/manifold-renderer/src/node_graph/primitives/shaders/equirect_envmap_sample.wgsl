// node.equirect_envmap_sample — per-pixel IBL reflection with Schlick
// Fresnel + roughness scaling, the IBL term of a Cook-Torrance PBR sum.
//
// For each pixel:
//   R = reflect(-V, N)
//   env = sample(env_map, equirect_uv(R))
//   F = F_schlick(NdotV, F0 = mix(0.04, base_color, metallic))
//   out = env * F * (1 - roughness * roughness_scale)
//
// Two modes (picked by `use_world_pos`):
//   0 = flat screen-space: V from constant `view` uniform; N is tangent-space.
//   1 = 3D mesh: V from `world_pos` texture + `view` (camera world pos);
//       N is world-space. Background pixels (world_pos.a < 0.5) emit zero.
//
// Bindings:
//   @binding(0) uniforms (EnvSampleUniforms)
//   @binding(1) tex_normal
//   @binding(2) tex_env
//   @binding(3) sampler (shared)
//   @binding(4) output_tex (rgba16float storage)
//   @binding(5) tex_world_pos (Rgba16Float; ignored when use_world_pos=0)
//   @binding(6) tex_roughness_map (sampled .r supersedes scalar roughness
//                                  when use_roughness_map=1)
//
// Prepended at pipeline-creation: pbr_brdf.wgsl
// (pbr_equirect_uv, pbr_f_schlick).

struct Uniforms {
    view: vec3<f32>,
    roughness: f32,
    base_color: vec4<f32>,
    metallic: f32,
    use_world_pos: u32,
    roughness_scale: f32,
    use_roughness_map: u32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_normal: texture_2d<f32>;
@group(0) @binding(2) var tex_env: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(5) var tex_world_pos: texture_2d<f32>;
@group(0) @binding(6) var tex_roughness_map: texture_2d<f32>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let N = normalize(textureSampleLevel(tex_normal, tex_sampler, uv, 0.0).rgb + vec3<f32>(1e-8));

    var V: vec3<f32>;
    if uniforms.use_world_pos == 1u {
        let wp_sample = textureSampleLevel(tex_world_pos, tex_sampler, uv, 0.0);
        if wp_sample.a < 0.5 {
            textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(0.0));
            return;
        }
        V = normalize(uniforms.view - wp_sample.xyz + vec3<f32>(1e-8));
    } else {
        V = normalize(uniforms.view + vec3<f32>(1e-8));
    }

    let R = reflect(-V, N);
    let env_uv = pbr_equirect_uv(R);
    let env = textureSampleLevel(tex_env, tex_sampler, env_uv, 0.0).rgb;

    let F0 = mix(vec3<f32>(0.04), uniforms.base_color.rgb, uniforms.metallic);
    let NdotV = max(dot(N, V), 0.001);
    let F_env = pbr_f_schlick(NdotV, F0);

    var roughness: f32 = uniforms.roughness;
    if uniforms.use_roughness_map == 1u {
        roughness = textureSampleLevel(tex_roughness_map, tex_sampler, uv, 0.0).r;
    }
    let env_factor = 1.0 - roughness * uniforms.roughness_scale;
    let ibl = env * F_env * env_factor;

    let scalar = max(max(ibl.x, ibl.y), ibl.z);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(ibl, scalar));
}
