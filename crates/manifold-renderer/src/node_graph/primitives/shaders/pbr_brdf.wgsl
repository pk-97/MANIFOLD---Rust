// pbr_brdf.wgsl — shared Cook-Torrance microfacet BRDF helpers + equirect
// environment-map sampling. Prepended at pipeline-creation time to any
// PBR-IBL primitive so all consumers compute identical math bit-for-bit.
//
// Conventions:
//   N, L, V — normalised surface normal / light dir / view dir (toward eye)
//   H       — half vector (normalised L+V)
//   F0      — base reflectance at normal incidence (vec3, metallic-aware)
//   roughness ∈ [0, 1]
//
// All helpers operate on cosines (dot products), expected non-negative
// where required. Callers clamp NdotL / NdotV / NdotH at the use site.

const PBR_PI: f32 = 3.14159265358979;

// GGX / Trowbridge-Reitz normal-distribution function.
fn pbr_d_ggx(NdotH: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let denom = NdotH * NdotH * (a2 - 1.0) + 1.0;
    return a2 / (PBR_PI * denom * denom);
}

// Schlick-GGX geometry term (one direction); paired into Smith below.
fn pbr_g_schlick_ggx(NdotV: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return NdotV / (NdotV * (1.0 - k) + k);
}

// Smith geometry function — masking + shadowing for both view and light.
fn pbr_g_smith(NdotV: f32, NdotL: f32, roughness: f32) -> f32 {
    return pbr_g_schlick_ggx(NdotV, roughness) * pbr_g_schlick_ggx(NdotL, roughness);
}

// Schlick Fresnel approximation. F0 is the base reflectance at normal
// incidence (dielectrics ~ 0.04, metals = base_color).
fn pbr_f_schlick(cosTheta: f32, F0: vec3<f32>) -> vec3<f32> {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

// Full Cook-Torrance specular for one directional light. Returns the
// `D * G * F / (4 * NdotV * NdotL)` term, NOT multiplied by light colour
// or NdotL — the caller applies those (so the same helper feeds direct
// lighting and IBL paths consistently).
fn pbr_cook_torrance_specular(
    N: vec3<f32>,
    L: vec3<f32>,
    V: vec3<f32>,
    roughness: f32,
    F0: vec3<f32>,
) -> vec3<f32> {
    let H = normalize(L + V);
    let NdotL = max(dot(N, L), 0.0);
    let NdotV = max(dot(N, V), 0.001);
    let NdotH = max(dot(N, H), 0.0);
    let VdotH = max(dot(V, H), 0.001);
    let D = pbr_d_ggx(NdotH, roughness);
    let G = pbr_g_smith(NdotV, NdotL, roughness);
    let F = pbr_f_schlick(VdotH, F0);
    return (D * G * F) / (4.0 * NdotV * NdotL + 0.0001);
}

// Convert a 3D direction to equirectangular (longitude/latitude) UV for
// sampling a 2D equirect environment map. Direction is assumed
// normalised; caller is responsible for `reflect(-V, N)` etc.
fn pbr_equirect_uv(dir: vec3<f32>) -> vec2<f32> {
    let azimuth = atan2(dir.z, dir.x);
    let elevation = asin(clamp(dir.y, -1.0, 1.0));
    return vec2<f32>(
        azimuth / (2.0 * PBR_PI) + 0.5,
        elevation / PBR_PI + 0.5,
    );
}
