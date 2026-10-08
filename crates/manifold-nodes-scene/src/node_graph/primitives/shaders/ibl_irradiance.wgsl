// node.render_scene internal pass — cosine-convolved diffuse irradiance
// map (IMPORT_FIDELITY_DESIGN.md D2/F-P1). PARTIAL: prepends
// `pbr_brdf.wgsl` at pipeline-creation time (pbr_hammersley /
// pbr_cosine_sample_hemisphere / pbr_dir_from_equirect_uv / pbr_equirect_uv
// all live there) — does not validate standalone, see
// `wgsl_validation.rs`'s composed validator.
//
// One dispatch, whole texture (32x16 default — `render_scene.rs`'s
// `ensure_irradiance_map`). Stores `average(radiance)` over
// `IRRADIANCE_SAMPLES` cosine-weighted hemisphere samples per texel — under
// cosine-weighted importance sampling the cos(theta) and 1/pi pdf terms
// cancel exactly, so this average IS the diffuse IBL term the fragment
// shader multiplies directly by `kd * albedo` (see `pbr_brdf.wgsl`'s
// `pbr_cosine_sample_hemisphere` doc comment — no extra pi scaling at
// either bake or sample time). 512 samples per texel — the F-P1 committed
// default.

struct IrradianceUniforms {
    dst_width: u32,
    dst_height: u32,
    src_width: u32,
    src_height: u32,
}

@group(0) @binding(0) var<uniform> u: IrradianceUniforms;
@group(0) @binding(1) var src_envmap: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var dst_irradiance: texture_storage_2d<rgba16float, write>;

const IRRADIANCE_SAMPLES: u32 = 512u;

@compute @workgroup_size(8, 8)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u.dst_width || gid.y >= u.dst_height {
        return;
    }
    let uv = vec2<f32>(
        (f32(gid.x) + 0.5) / f32(u.dst_width),
        (f32(gid.y) + 0.5) / f32(u.dst_height),
    );
    let N = pbr_dir_from_equirect_uv(uv);

    var sum = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < IRRADIANCE_SAMPLES; i = i + 1u) {
        let xi = pbr_hammersley(i, IRRADIANCE_SAMPLES);
        let L = pbr_cosine_sample_hemisphere(xi, N);
        let sample_uv = pbr_equirect_uv(L);
        sum = sum + textureSampleLevel(src_envmap, src_sampler, sample_uv, 0.0).rgb;
    }
    let result = sum / f32(IRRADIANCE_SAMPLES);
    textureStore(dst_irradiance, vec2<i32>(gid.xy), vec4<f32>(result, 1.0));
}
