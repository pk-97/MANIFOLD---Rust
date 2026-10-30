// node.bake_environment's Daylight mode: the sky the Sun makes through the
// air (`daylight.wgsl`), baked into the equirect environment map in the same
// layout as the other modes, so lighting, reflections and the background
// all see it. Radiance is per unit of the sun's light above the air, times
// that light, so sky and Sun stay in balance at every height of the sun.
//
// `ms_main` fills the multiple-scattering table once (Hillaire 2020, 5.5):
// for each height and sun angle, the light scattered more than once,
// treated as isotropic. `sky_main` marches each texel's view ray, adding
// single scattering toward the sun, the table's multiple scattering, and
// below the horizon the lit ground.
//
// Bindings: 0 uniforms, 1 output (rgba16float), 2 atmosphere, 3 the table.

struct DaylightUniforms {
    width: u32,
    height: u32,
    // Master brightness of the whole sky.
    intensity: f32,
    // The sun disc's brightness; 0 draws none, so the Sun lights the scene
    // only through its own light.
    sun_disc_intensity: f32,
    // Direction toward the sun; w the disc's angular radius.
    sun: vec4<f32>,
    // The sun's light above the air: the Sun light's colour times its
    // Intensity.
    sun_light: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: DaylightUniforms;
@group(0) @binding(1) var dst_tex: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<uniform> air: Atmosphere;
@group(0) @binding(3) var<storage, read_write> ms_table: array<vec4<f32>>;

// The table's side: sun angle across, height down.
const MS_SIZE: u32 = 32u;
// Directions per table entry (MS_SIDE squared) and steps along each.
const MS_SIDE: u32 = 8u;
const MS_STEPS: i32 = 20;
// Steps along a sky texel's view ray, packed toward the viewer, where the
// air is densest.
const SKY_STEPS: i32 = 32;

@compute @workgroup_size(8, 8)
fn ms_main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= MS_SIZE || id.y >= MS_SIZE {
        return;
    }
    let mu_s = (f32(id.x) + 0.5) / f32(MS_SIZE) * 2.0 - 1.0;
    let h = (f32(id.y) + 0.5) / f32(MS_SIZE) * (air.radii.y - air.radii.x);
    let r = air.radii.x + h;
    let sun = vec3<f32>(sqrt(max(1.0 - mu_s * mu_s, 0.0)), mu_s, 0.0);
    let origin = vec3<f32>(0.0, r, 0.0);
    var second = vec3<f32>(0.0);
    var transfer = vec3<f32>(0.0);
    for (var i = 0u; i < MS_SIDE; i++) {
        for (var j = 0u; j < MS_SIDE; j++) {
            // Even directions over the sphere: even in cos(theta) and phi.
            let cos_theta = 1.0 - 2.0 * (f32(i) + 0.5) / f32(MS_SIDE);
            let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
            let phi = 2.0 * DAYLIGHT_PI * (f32(j) + 0.5) / f32(MS_SIDE);
            let dir = vec3<f32>(sin_theta * cos(phi), cos_theta, sin_theta * sin(phi));
            let reach = ray_length(air, r, cos_theta);
            let dt = reach / f32(MS_STEPS);
            var view = vec3<f32>(1.0);
            var lit = vec3<f32>(0.0);
            var through = vec3<f32>(0.0);
            for (var k = 0; k < MS_STEPS; k++) {
                let p = origin + dir * ((f32(k) + 0.5) * dt);
                let pr = length(p);
                let a = air_at(air, pr - air.radii.x);
                let scattering = a.rayleigh + vec3<f32>(a.mie);
                let step = exp(-a.extinction * dt);
                let gathered = (vec3<f32>(1.0) - step) / max(a.extinction, vec3<f32>(1e-9));
                let to_sun = transmittance_to_space(air, pr, dot(p, sun) / pr);
                lit += view * scattering * to_sun / (4.0 * DAYLIGHT_PI) * gathered;
                through += view * scattering * gathered;
                view *= step;
            }
            if meets_ground(air, r, cos_theta) {
                let p = origin + dir * reach;
                let up = normalize(p);
                let to_sun = transmittance_to_space(air, air.radii.x, dot(up, sun));
                lit += view * to_sun * max(dot(up, sun), 0.0) * air.radii.w / DAYLIGHT_PI;
            }
            second += lit;
            transfer += through;
        }
    }
    let count = f32(MS_SIDE * MS_SIDE);
    second /= count;
    transfer /= count;
    ms_table[id.y * MS_SIZE + id.x] = vec4<f32>(second / max(vec3<f32>(1.0) - transfer, vec3<f32>(1e-3)), 0.0);
}

// Multiple scattering at radius `r` under a sun at cosine `mu_s`, read from
// the table with bilinear filtering.
fn multiple_scattering(r: f32, mu_s: f32) -> vec3<f32> {
    let x = clamp((mu_s * 0.5 + 0.5) * f32(MS_SIZE) - 0.5, 0.0, f32(MS_SIZE - 1u));
    let y = clamp((r - air.radii.x) / (air.radii.y - air.radii.x) * f32(MS_SIZE) - 0.5, 0.0, f32(MS_SIZE - 1u));
    let x0 = u32(floor(x));
    let y0 = u32(floor(y));
    let x1 = min(x0 + 1u, MS_SIZE - 1u);
    let y1 = min(y0 + 1u, MS_SIZE - 1u);
    let f = vec2<f32>(x - floor(x), y - floor(y));
    let top = mix(ms_table[y0 * MS_SIZE + x0].rgb, ms_table[y0 * MS_SIZE + x1].rgb, f.x);
    let bottom = mix(ms_table[y1 * MS_SIZE + x0].rgb, ms_table[y1 * MS_SIZE + x1].rgb, f.x);
    return mix(top, bottom, f.y);
}

@compute @workgroup_size(16, 16)
fn sky_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= u.width || gid.y >= u.height {
        return;
    }
    // The same equirect layout as `bake_equirect_envmap.wgsl`.
    let azimuth = f32(gid.x) / f32(u.width) * 2.0 * DAYLIGHT_PI - DAYLIGHT_PI;
    let elevation = f32(gid.y) / f32(u.height) * DAYLIGHT_PI - DAYLIGHT_PI * 0.5;
    let cos_e = cos(elevation);
    let dir = vec3<f32>(cos_e * cos(azimuth), sin(elevation), cos_e * sin(azimuth));
    let sun = normalize(u.sun.xyz);
    let r = air.radii.x + air.radii.z;
    let origin = vec3<f32>(0.0, r, 0.0);
    let reach = ray_length(air, r, dir.y);
    let c = dot(dir, sun);
    let phase_r = rayleigh_phase(c);
    let phase_m = mie_phase(c, air.mie.w);

    var view = vec3<f32>(1.0);
    var radiance = vec3<f32>(0.0);
    var t0 = 0.0;
    for (var k = 0; k < SKY_STEPS; k++) {
        let s = (f32(k) + 1.0) / f32(SKY_STEPS);
        let t1 = reach * s * s;
        let dt = t1 - t0;
        let p = origin + dir * (0.5 * (t0 + t1));
        t0 = t1;
        let pr = length(p);
        let mu_s = dot(p, sun) / pr;
        let a = air_at(air, pr - air.radii.x);
        let to_sun = transmittance_to_space(air, pr, mu_s);
        let scattered = (a.rayleigh * phase_r + vec3<f32>(a.mie * phase_m)) * to_sun
            + (a.rayleigh + vec3<f32>(a.mie)) * multiple_scattering(pr, mu_s);
        let step = exp(-a.extinction * dt);
        // Hillaire's per-step integral of light scattered in over the step,
        // dimmed by the step's own air.
        radiance += view * scattered * (vec3<f32>(1.0) - step) / max(a.extinction, vec3<f32>(1e-9));
        view *= step;
    }
    if meets_ground(air, r, dir.y) {
        let up = normalize(origin + dir * reach);
        let to_sun = transmittance_to_space(air, air.radii.x, dot(up, sun));
        radiance += view * to_sun * max(dot(up, sun), 0.0) * air.radii.w / DAYLIGHT_PI;
    } else if u.sun_disc_intensity > 0.0 {
        let disc = smoothstep(cos(max(u.sun.w, 1e-4)), 1.0, c);
        radiance += view * 4.0 * u.sun_disc_intensity * disc;
    }
    let color = radiance * u.sun_light.rgb * u.intensity;
    textureStore(dst_tex, vec2<i32>(gid.xy), vec4<f32>(color, 1.0));
}
