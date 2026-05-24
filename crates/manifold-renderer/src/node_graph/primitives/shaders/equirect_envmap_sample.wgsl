// node.equirect_envmap_sample — per-pixel IBL reflection.
//
// For each pixel: R = reflect(-view, normal); sample env_map at
// equirectangular UV from R.
//
// Bindings:
//   @binding(0) uniforms (16 bytes — view + pad)
//   @binding(1) tex_normal
//   @binding(2) tex_env
//   @binding(3) sampler (shared)
//   @binding(4) output_tex (rgba16float storage)
//
// Prepended at pipeline-creation: pbr_brdf.wgsl (pbr_equirect_uv).

struct Uniforms {
    view: vec3<f32>,
    _pad0: f32,
}

@group(0) @binding(0) var<uniform> uniforms: Uniforms;
@group(0) @binding(1) var tex_normal: texture_2d<f32>;
@group(0) @binding(2) var tex_env: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let N = normalize(textureSampleLevel(tex_normal, tex_sampler, uv, 0.0).rgb + vec3<f32>(1e-8));
    let V = normalize(uniforms.view + vec3<f32>(1e-8));
    let R = reflect(-V, N);
    let env_uv = pbr_equirect_uv(R);
    let env = textureSampleLevel(tex_env, tex_sampler, env_uv, 0.0).rgb;

    let scalar = max(max(env.x, env.y), env.z);
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(env, scalar));
}
