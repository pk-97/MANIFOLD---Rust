// node.render_scene internal pass — split-sum BRDF LUT (Karis 2013, "Real
// Shading in Unreal Engine 4"), IMPORT_FIDELITY_DESIGN.md D2/F-P1. PARTIAL:
// prepends `pbr_brdf.wgsl` at pipeline-creation time (pbr_hammersley /
// pbr_importance_sample_ggx / pbr_g_schlick_ggx_k all live there) — does
// not validate standalone, see `wgsl_validation.rs`'s composed validator.
//
// 128x128 rg16float: x = NdotV in [0,1], y = roughness in [0,1]. Computed
// ONCE PER DEVICE — envmap-independent (view/roughness-only) — and cached
// forever by `render_scene.rs::ensure_brdf_lut`. 1024 samples per texel —
// the F-P1 committed default. Specular IBL becomes
// `prefiltered * (F0 * lut.x + lut.y)` at the fragment shader.

struct LutUniforms {
    width: u32,
    height: u32,
    _pad0: f32,
    _pad1: f32,
}

@group(0) @binding(0) var<uniform> u: LutUniforms;
@group(0) @binding(1) var dst_lut: texture_storage_2d<rg16float, write>;

const LUT_SAMPLES: u32 = 1024u;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u.width || gid.y >= u.height {
        return;
    }
    let NdotV = max((f32(gid.x) + 0.5) / f32(u.width), 0.001);
    let roughness = max((f32(gid.y) + 0.5) / f32(u.height), 0.01);

    // Tangent-space view vector reconstructed from NdotV alone (N = +Z) —
    // the standard split-sum LUT setup: the result only depends on the
    // angle between V and N, not V's absolute orientation.
    let V = vec3<f32>(sqrt(1.0 - NdotV * NdotV), 0.0, NdotV);
    let N = vec3<f32>(0.0, 0.0, 1.0);

    var A = 0.0;
    var B = 0.0;
    // IBL geometry-term k (distinct from the direct-lighting k in
    // pbr_g_schlick_ggx — see pbr_brdf.wgsl's pbr_g_schlick_ggx_k doc).
    let k = (roughness * roughness) / 2.0;
    for (var i: u32 = 0u; i < LUT_SAMPLES; i = i + 1u) {
        let xi = pbr_hammersley(i, LUT_SAMPLES);
        let H = pbr_importance_sample_ggx(xi, roughness, N);
        let L = normalize(2.0 * dot(V, H) * H - V);
        let NdotL = max(L.z, 0.0);
        let NdotH = max(H.z, 0.0);
        let VdotH = max(dot(V, H), 0.0);
        if NdotL > 0.0 {
            let g_vis_denom = max(NdotH * NdotV, 1e-4);
            let g = pbr_g_schlick_ggx_k(NdotL, k) * pbr_g_schlick_ggx_k(NdotV, k);
            let g_vis = (g * VdotH) / g_vis_denom;
            let fc = pow(clamp(1.0 - VdotH, 0.0, 1.0), 5.0);
            A = A + (1.0 - fc) * g_vis;
            B = B + fc * g_vis;
        }
    }
    A = A / f32(LUT_SAMPLES);
    B = B / f32(LUT_SAMPLES);
    textureStore(dst_lut, vec2<i32>(gid.xy), vec4<f32>(A, B, 0.0, 0.0));
}
