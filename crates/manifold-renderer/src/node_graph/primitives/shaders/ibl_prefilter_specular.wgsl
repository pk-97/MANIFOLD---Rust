// node.render_scene internal pass — GGX-importance-convolved specular
// prefilter mip (IMPORT_FIDELITY_DESIGN.md D2/F-P1). PARTIAL: prepends
// `pbr_brdf.wgsl` at pipeline-creation time (pbr_hammersley /
// pbr_importance_sample_ggx / pbr_dir_from_equirect_uv / pbr_equirect_uv /
// PBR_PI all live there) — does not validate standalone, see
// `wgsl_validation.rs`'s composed validator.
//
// One dispatch per mip level: `render_scene.rs::ensure_prefiltered_chain`
// runs this once per mip with `roughness = mip_index / (PREFILTER_MIP_COUNT
// - 1)` (mip 0 = perfect mirror, the last mip = fully rough), writing into
// a single-mip VIEW of the destination chain
// (`GpuTexture::mip_level_view`). 256 importance samples per texel — the
// F-P1 committed default (IMPORT_FIDELITY_DESIGN.md §5, "change only if
// the cost measurement exceeds 10ms").

struct PrefilterUniforms {
    dst_width: u32,
    dst_height: u32,
    src_width: u32,
    src_height: u32,
    roughness: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> u: PrefilterUniforms;
@group(0) @binding(1) var src_envmap: texture_2d<f32>;
@group(0) @binding(2) var src_sampler: sampler;
@group(0) @binding(3) var dst_mip: texture_storage_2d<rgba16float, write>;

const PREFILTER_SAMPLES: u32 = 256u;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u.dst_width || gid.y >= u.dst_height {
        return;
    }
    let uv = vec2<f32>(
        (f32(gid.x) + 0.5) / f32(u.dst_width),
        (f32(gid.y) + 0.5) / f32(u.dst_height),
    );
    // Split-sum assumption N = V = R (Karis 2013) — the standard
    // simplification that lets the prefiltered map be indexed by
    // direction alone, independent of view angle.
    let N = pbr_dir_from_equirect_uv(uv);
    let roughness = max(u.roughness, 0.01);

    var color_sum = vec3<f32>(0.0);
    var weight_sum = 0.0;
    for (var i: u32 = 0u; i < PREFILTER_SAMPLES; i = i + 1u) {
        let xi = pbr_hammersley(i, PREFILTER_SAMPLES);
        let H = pbr_importance_sample_ggx(xi, roughness, N);
        let L = normalize(2.0 * dot(N, H) * H - N); // reflect(-N, H), V=N
        let NdotL = dot(N, L);
        if NdotL > 0.0 {
            let sample_uv = pbr_equirect_uv(L);
            let c = textureSampleLevel(src_envmap, src_sampler, sample_uv, 0.0).rgb;
            color_sum = color_sum + c * NdotL;
            weight_sum = weight_sum + NdotL;
        }
    }

    var result: vec3<f32>;
    if weight_sum > 0.0 {
        result = color_sum / weight_sum;
    } else {
        // N is always a valid unit direction, so every sample's NdotL<=0
        // case only degenerates at extreme roughness with unlucky
        // sampling; fall back to a direct lookup so the texel is never
        // NaN/zero rather than leaving it undefined.
        result = textureSampleLevel(src_envmap, src_sampler, uv, 0.0).rgb;
    }
    textureStore(dst_mip, vec2<i32>(gid.xy), vec4<f32>(result, 1.0));
}
