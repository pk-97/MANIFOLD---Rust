// node.radial_burst_force_field — per-pixel vec2 force texture
// for a radial+tangent impulse burst around (point_x, point_y).
//
// (`noise_common.wgsl` is prepended at pipeline creation — provides
// the Ashima `simplex3d` for the noise-perturbed radial direction.)
//
// Per pixel:
//   delta = uv - point
//   dist  = length(delta)
//   if dist > radius || dist < eps || amplitude * envelope < eps:
//       force = (0, 0)
//   else:
//       t = dist / radius
//       radial = delta / dist
//       tangent = (-radial.y, radial.x)
//       falloff = (1 - t²)²
//       noise_angle = simplex3d(uv*8 + time*0.3) * PI
//       perturbed_radial = normalize(radial + (cos(angle), sin(angle)) * 0.4 * t)
//       curl_profile = t * (1 - t) * 4
//       strength = amplitude * envelope * falloff
//       force = perturbed_radial * strength + tangent * curl_profile * strength * 0.5
//   textureStore(uv, vec4(force, 0, 1))

struct Uniforms {
    point_x: f32,
    point_y: f32,
    amplitude: f32,
    envelope: f32,
    radius: f32,
    time_val: f32,
    _pad0: f32,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

const PI: f32 = 3.14159265;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);

    let amp_env = u.amplitude * u.envelope;
    let radius = max(u.radius, 1.0e-6);

    var force = vec2<f32>(0.0, 0.0);

    let delta = uv - vec2<f32>(u.point_x, u.point_y);
    let dist2 = dot(delta, delta);
    let radius2 = radius * radius;

    if dist2 < radius2 && dist2 > 1.0e-8 && amp_env > 1.0e-4 {
        let dist = sqrt(dist2);
        let t = dist / radius;
        let radial = delta / dist;
        let tangent = vec2<f32>(-radial.y, radial.x);

        let one_minus_t2 = 1.0 - t * t;
        let falloff = one_minus_t2 * one_minus_t2;

        let noise_seed = vec3<f32>(uv * 8.0 + u.time_val * 0.3, 0.0);
        let noise_angle = simplex3d(noise_seed) * PI;
        let noise_dir = vec2<f32>(cos(noise_angle), sin(noise_angle));
        let perturbed_radial = normalize(radial + noise_dir * 0.4 * t);

        let curl_profile = t * (1.0 - t) * 4.0;

        let strength = amp_env * falloff;
        force = perturbed_radial * strength + tangent * curl_profile * strength * 0.5;
    }

    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(force, 0.0, 1.0));
}
