// Look hook: mist from spray. Thin fog of droplets in the air above the water.
// The mist field (`water_mist.wgsl` integrates it per pixel) holds the
// extinction coefficient per metre at each node; along a camera ray the
// optical depth is `tau`, and the light is `behind * exp(-tau) + L_in * (1 -
// exp(-tau))`: single scattering with uniform lighting along the ray.
// `fs_mist` draws the second term and lets the blend apply the first.

// Water droplets in visible light absorb almost nothing, so every extinction is
// a scatter.
const MIST_ALBEDO: f32 = 1.0;

// Fog and cloud droplets scatter forward, with a Henyey-Greenstein asymmetry
// near 0.85 (Hansen and Travis 1974, "Light scattering in planetary
// atmospheres").
const MIST_ASYMMETRY: f32 = 0.85;

// The Henyey-Greenstein phase function, normalised over the sphere.
fn mist_phase(cos_theta: f32) -> f32 {
    let g = MIST_ASYMMETRY;
    let denom = 1.0 + g * g - 2.0 * g * cos_theta;
    return (1.0 - g * g) / (4.0 * PI * pow(denom, 1.5));
}

// The mist's colour along a ray as (premultiplied colour, coverage): the
// in-scattered light times `1 - T` and `1 - T`, with `T = exp(-tau)`.
// `mean_pos` is the mean position of the mist along the ray, where the lights
// and their shadows are read.
fn look_mist(
    ray_dir: vec3<f32>,
    mean_pos: vec3<f32>,
    frag_xy: vec2<f32>,
    tau: f32,
) -> vec4<f32> {
    // The irradiance map holds what a white Lambert surface facing `n` returns,
    // so its mean over the six axis directions is the mean radiance an
    // isotropic mist sees.
    var ambient = ibl_irradiance(vec3<f32>(1.0, 0.0, 0.0), frag_xy);
    ambient += ibl_irradiance(vec3<f32>(-1.0, 0.0, 0.0), frag_xy);
    ambient += ibl_irradiance(vec3<f32>(0.0, 1.0, 0.0), frag_xy);
    ambient += ibl_irradiance(vec3<f32>(0.0, -1.0, 0.0), frag_xy);
    ambient += ibl_irradiance(vec3<f32>(0.0, 0.0, 1.0), frag_xy);
    ambient += ibl_irradiance(vec3<f32>(0.0, 0.0, -1.0), frag_xy);
    ambient = ambient / 6.0;

    var scattered = ambient;
    let light_count = u32(u.scene_params.x);
    for (var i = 0u; i < light_count; i = i + 1u) {
        let l_dir = light_direction_attenuation(i, mean_pos);
        let l_col = lights[i * LIGHT_STRIDE + 1u];
        // Looking toward a light (the ray along the direction to it) is
        // forward scattering.
        let phase = mist_phase(dot(l_dir.xyz, ray_dir));
        let vis = light_visibility(l_col.w, mean_pos, frag_xy);
        scattered += phase * l_col.rgb * l_dir.w * vis;
    }
    let l_in = MIST_ALBEDO * scattered;
    let transmittance = exp(-tau);
    return vec4<f32>(l_in * (1.0 - transmittance), 1.0 - transmittance);
}
