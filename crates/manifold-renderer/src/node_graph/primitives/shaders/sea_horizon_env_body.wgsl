// node.sea_horizon_env — fusable body (freeze section 12), GATHER. An
// equirect environment seen from a calm sea: the upper hemisphere passes
// through; a ray below the horizon (depression φ) meets the sea, which
// reflects the mirrored sky by Schlick's Fresnel at that grazing angle and
// shows the water's own colour for the rest. The renderer's env convention
// is v = elevation/π + 0.5, so v < 0.5 is below the horizon.
// PARAMS: [ior, water_r, water_g, water_b].
fn body(tex_sky: texture_2d<f32>, samp: sampler, uv: vec2<f32>, dims: vec2<f32>, ior: f32, water_r: f32, water_g: f32, water_b: f32) -> vec4<f32> {
    let pi = 3.14159265358979;
    let depression = (0.5 - uv.y) * pi;
    if depression <= 0.0 {
        return vec4<f32>(textureSampleLevel(tex_sky, samp, uv, 0.0).rgb, 1.0);
    }
    let sky = textureSampleLevel(tex_sky, samp, vec2<f32>(uv.x, 1.0 - uv.y), 0.0).rgb;
    let r0 = (ior - 1.0) / (ior + 1.0);
    let f0 = r0 * r0;
    let fresnel = f0 + (1.0 - f0) * pow(1.0 - sin(depression), 5.0);
    return vec4<f32>(fresnel * sky + (1.0 - fresnel) * vec3<f32>(water_r, water_g, water_b), 1.0);
}
