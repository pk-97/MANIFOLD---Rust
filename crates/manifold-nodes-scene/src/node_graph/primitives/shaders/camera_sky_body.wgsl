// node.camera_sky — fusable body (freeze section 12), GATHER. The equirect
// environment as the camera sees it: each pixel's camera ray, looked up with
// render_scene's own env convention (u = atan2(z, x)/2π + 0.5,
// v = asin(y)/π + 0.5), so the sky lines up with the scene's reflections.
// PARAMS: none; the camera basis and tan(fov_y/2) arrive derived.
fn body(
    tex_sky: texture_2d<f32>,
    samp: sampler,
    uv: vec2<f32>,
    dims: vec2<f32>,
    fwd_x: f32,
    fwd_y: f32,
    fwd_z: f32,
    right_x: f32,
    right_y: f32,
    right_z: f32,
    up_x: f32,
    up_y: f32,
    up_z: f32,
    tan_y: f32,
) -> vec4<f32> {
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let aspect = dims.x / max(dims.y, 1.0);
    let d = normalize(vec3<f32>(fwd_x, fwd_y, fwd_z)
        + ndc.x * tan_y * aspect * vec3<f32>(right_x, right_y, right_z)
        + ndc.y * tan_y * vec3<f32>(up_x, up_y, up_z));
    let pi = 3.14159265358979;
    let env_uv = vec2<f32>(fract(atan2(d.z, d.x) / (2.0 * pi) + 0.5), asin(clamp(d.y, -1.0, 1.0)) / pi + 0.5);
    return vec4<f32>(textureSampleLevel(tex_sky, samp, env_uv, 0.0).rgb, 1.0);
}
