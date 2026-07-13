// node.explosion_force — fusable TEXTURE body (freeze §12, texture domain),
// SOURCE. Per-pixel vec2 force field for a radial+tangent impulse burst
// around (point_x, point_y) within `radius`, with a noise-perturbed radial
// direction. Matches radial_burst_force_field.wgsl bit-for-bit. `simplex3d`
// comes from `noise_common.wgsl`, threaded via this primitive's
// `wgsl_includes: [NOISE_COMMON]` (same source as
// node.simplex_noise_per_copy / node.simplex_field_2d — bit-exact noise).
//
// ABI (texture standalone/fused codegen, Source): no texture input, so the
// body takes (uv, dims, <params...>) and returns the vec4 written to the
// output texture (force in .xy, 0 in .z, 1 in .w — matches the hand kernel).
const RBF_PI: f32 = 3.14159265;

fn body(
    uv: vec2<f32>,
    dims: vec2<f32>,
    point_x: f32,
    point_y: f32,
    amplitude: f32,
    envelope: f32,
    radius: f32,
    time_val: f32,
) -> vec4<f32> {
    let amp_env = amplitude * envelope;
    let r = max(radius, 1.0e-6);

    var force = vec2<f32>(0.0, 0.0);

    let delta = uv - vec2<f32>(point_x, point_y);
    let dist2 = dot(delta, delta);
    let radius2 = r * r;

    if dist2 < radius2 && dist2 > 1.0e-8 && amp_env > 1.0e-4 {
        let dist = sqrt(dist2);
        let t = dist / r;
        let radial = delta / dist;
        let tangent = vec2<f32>(-radial.y, radial.x);

        let one_minus_t2 = 1.0 - t * t;
        let falloff = one_minus_t2 * one_minus_t2;

        let noise_seed = vec3<f32>(uv * 8.0 + time_val * 0.3, 0.0);
        let noise_angle = simplex3d(noise_seed) * RBF_PI;
        let noise_dir = vec2<f32>(cos(noise_angle), sin(noise_angle));
        let perturbed_radial = normalize(radial + noise_dir * 0.4 * t);

        let curl_profile = t * (1.0 - t) * 4.0;

        let strength = amp_env * falloff;
        force = perturbed_radial * strength + tangent * curl_profile * strength * 0.5;
    }

    return vec4<f32>(force, 0.0, 1.0);
}
