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

// Inverse of `pbr_equirect_uv`: reconstruct the unit direction a given
// equirect texel UV represents. Used by the IBL convolution passes
// (IMPORT_FIDELITY_DESIGN.md D2/F-P1) to turn a destination texel's UV
// into the N (irradiance) or R (prefilter) direction being baked.
fn pbr_dir_from_equirect_uv(uv: vec2<f32>) -> vec3<f32> {
    let azimuth = (uv.x - 0.5) * 2.0 * PBR_PI;
    let elevation = (uv.y - 0.5) * PBR_PI;
    let ce = cos(elevation);
    return vec3<f32>(ce * cos(azimuth), sin(elevation), ce * sin(azimuth));
}

// ===== Split-sum IBL helpers (IMPORT_FIDELITY_DESIGN.md D2/F-P1) =====
// Shared by the prefiltered-specular, diffuse-irradiance, and BRDF-LUT
// convolution passes (`shaders/ibl_prefilter_specular.wgsl`,
// `shaders/ibl_irradiance.wgsl`, `shaders/ibl_brdf_lut.wgsl`) — prepended
// at pipeline-creation time via `concat!(include_str!(...), ...)`,
// matching this file's header contract.

// Van der Corput radical inverse (base 2, bit-reversal trick — Karis 2013
// "Real Shading in Unreal Engine 4" / Hacker's Delight). Paired with `i`
// below to build a low-discrepancy Hammersley sequence for GGX importance
// sampling.
fn pbr_radical_inverse_vdc(bits_in: u32) -> f32 {
    var bits = bits_in;
    bits = (bits << 16u) | (bits >> 16u);
    bits = ((bits & 0x55555555u) << 1u) | ((bits & 0xAAAAAAAAu) >> 1u);
    bits = ((bits & 0x33333333u) << 2u) | ((bits & 0xCCCCCCCCu) >> 2u);
    bits = ((bits & 0x0F0F0F0Fu) << 4u) | ((bits & 0xF0F0F0F0u) >> 4u);
    bits = ((bits & 0x00FF00FFu) << 8u) | ((bits & 0xFF00FF00u) >> 8u);
    return f32(bits) * 2.3283064365386963e-10; // bits / 2^32
}

fn pbr_hammersley(i: u32, n: u32) -> vec2<f32> {
    return vec2<f32>(f32(i) / f32(n), pbr_radical_inverse_vdc(i));
}

// GGX importance-sampled half-vector H, returned in WORLD space via a
// tangent frame built around `n`. `xi` is a Hammersley 2D sample.
fn pbr_importance_sample_ggx(xi: vec2<f32>, roughness: f32, n: vec3<f32>) -> vec3<f32> {
    let a = roughness * roughness;
    let phi = 2.0 * PBR_PI * xi.x;
    let cos_theta = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
    let sin_theta = sqrt(max(0.0, 1.0 - cos_theta * cos_theta));
    let h_tangent = vec3<f32>(sin_theta * cos(phi), sin_theta * sin(phi), cos_theta);
    let up = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(n.z) < 0.999);
    let tangent = normalize(cross(up, n));
    let bitangent = cross(n, tangent);
    return normalize(tangent * h_tangent.x + bitangent * h_tangent.y + n * h_tangent.z);
}

// Cosine-weighted hemisphere-sampled direction around `n` (tangent frame,
// same convention as `pbr_importance_sample_ggx`). Used by the diffuse
// irradiance convolution — under cosine-weighted importance sampling the
// cos(theta) and 1/pi pdf terms cancel exactly, so averaging
// `radiance(L)` over these samples already IS the diffuse IBL term
// (no extra pi scaling needed at either bake or sample time).
fn pbr_cosine_sample_hemisphere(xi: vec2<f32>, n: vec3<f32>) -> vec3<f32> {
    let r = sqrt(xi.x);
    let phi = 2.0 * PBR_PI * xi.y;
    let l_tangent = vec3<f32>(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - xi.x)));
    let up = select(vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 0.0, 1.0), abs(n.z) < 0.999);
    let tangent = normalize(cross(up, n));
    let bitangent = cross(n, tangent);
    return normalize(tangent * l_tangent.x + bitangent * l_tangent.y + n * l_tangent.z);
}

// Schlick-GGX geometry term with an EXPLICIT `k` — the split-sum BRDF LUT
// (Karis 2013) uses `k = roughness*roughness/2`, a different formula from
// `pbr_g_schlick_ggx` above's direct-lighting `k = (roughness+1)^2/8`.
// Kept as a separate helper (not a mode flag on the existing one) so each
// call site's `k` derivation stays visible at its own use.
fn pbr_g_schlick_ggx_k(NdotX: f32, k: f32) -> f32 {
    return NdotX / (NdotX * (1.0 - k) + k);
}
