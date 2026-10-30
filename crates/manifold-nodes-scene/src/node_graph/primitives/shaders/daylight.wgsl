// The Earth's air, as Sébastien Hillaire models it in "A Scalable and
// Production Ready Sky and Atmosphere Rendering Technique" (EGSR 2020),
// with Bruneton's constants ("Precomputed Atmospheric Scattering", 2017):
// a shell of air over a round ground, Rayleigh scattering from the gas,
// Mie scattering from haze, and ozone that absorbs. Distances are in km.
// `daylight.rs` packs the constants; `bake_daylight.wgsl` bakes the sky
// from them.

struct Atmosphere {
    // x ground radius, y top of the air, z the viewer's height above the
    // ground, w the ground's albedo.
    radii: vec4<f32>,
    // Rayleigh scattering per km (rgb); w its scale height.
    rayleigh: vec4<f32>,
    // x Mie scattering per km, y Mie extinction per km, z scale height,
    // w the phase's forward lean g.
    mie: vec4<f32>,
    // Ozone absorption per km (rgb); w the height of its peak. It falls
    // linearly to nothing OZONE_HALF_WIDTH either side.
    ozone: vec4<f32>,
};

const OZONE_HALF_WIDTH: f32 = 15.0;
const DAYLIGHT_PI: f32 = 3.14159265;
// Steps along a ray to the top of the air for its transmittance, packed
// toward its start (the step ends go as the square of their count), where
// haze a kilometre deep lives; `daylight.rs` steps the same way.
const TRANSMITTANCE_STEPS: i32 = 24;

struct Air {
    rayleigh: vec3<f32>,
    mie: f32,
    extinction: vec3<f32>,
};

// What the air at height `h` scatters and takes away, per km.
fn air_at(a: Atmosphere, h: f32) -> Air {
    let rayleigh = a.rayleigh.rgb * exp(-max(h, 0.0) / a.rayleigh.w);
    let haze = exp(-max(h, 0.0) / a.mie.z);
    let ozone = a.ozone.rgb * max(1.0 - abs(h - a.ozone.w) / OZONE_HALF_WIDTH, 0.0);
    return Air(rayleigh, a.mie.x * haze, rayleigh + vec3<f32>(a.mie.y * haze) + ozone);
}

// Whether a ray from radius `r` with cosine `mu` to straight up meets the
// ground.
fn meets_ground(a: Atmosphere, r: f32, mu: f32) -> bool {
    return mu < 0.0 && r * r * (mu * mu - 1.0) + a.radii.x * a.radii.x >= 0.0;
}

// How far a ray from radius `r` with cosine `mu` runs: to the ground if it
// meets it, else out of the top of the air.
fn ray_length(a: Atmosphere, r: f32, mu: f32) -> f32 {
    if meets_ground(a, r, mu) {
        return max(-r * mu - sqrt(max(r * r * (mu * mu - 1.0) + a.radii.x * a.radii.x, 0.0)), 0.0);
    }
    return max(-r * mu + sqrt(max(r * r * (mu * mu - 1.0) + a.radii.y * a.radii.y, 0.0)), 0.0);
}

// The share of sunlight that reaches radius `r` from a sun whose cosine to
// straight up is `mu`: none when the ground is in the way.
fn transmittance_to_space(a: Atmosphere, r: f32, mu: f32) -> vec3<f32> {
    if meets_ground(a, r, mu) {
        return vec3<f32>(0.0);
    }
    let reach = ray_length(a, r, mu);
    var depth = vec3<f32>(0.0);
    var t0 = 0.0;
    for (var i = 0; i < TRANSMITTANCE_STEPS; i++) {
        let s = (f32(i) + 1.0) / f32(TRANSMITTANCE_STEPS);
        let t1 = reach * s * s;
        let t = 0.5 * (t0 + t1);
        let h = sqrt(t * t + 2.0 * r * mu * t + r * r) - a.radii.x;
        depth += air_at(a, h).extinction * (t1 - t0);
        t0 = t1;
    }
    return exp(-depth);
}

fn rayleigh_phase(c: f32) -> f32 {
    return 3.0 / (16.0 * DAYLIGHT_PI) * (1.0 + c * c);
}

// Cornette and Shanks' Mie phase.
fn mie_phase(c: f32, g: f32) -> f32 {
    let g2 = g * g;
    return 3.0 / (8.0 * DAYLIGHT_PI) * (1.0 - g2) * (1.0 + c * c)
        / ((2.0 + g2) * pow(max(1.0 + g2 - 2.0 * g * c, 1e-4), 1.5));
}
