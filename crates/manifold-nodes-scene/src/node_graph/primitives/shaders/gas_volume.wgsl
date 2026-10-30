// Smoke and fire, marched through a gas box and blended over the frame. Starts
// after `scene_shading.wgsl` (the object uniform `u`, the lights, the shadow
// maps, the irradiance map and fog) and the generated constants
// `BLACKBODY_MIN_KELVIN`, `BLACKBODY_MAX_KELVIN`, `BLACKBODY_ENTRIES` and
// `NOISE_LATTICE_CELLS` (`gas_volume.rs`).
//
// Light units are the scene's. A light's premultiplied colour is the
// irradiance on a surface facing it, so a white Lambert surface shows
// colour / PI. A phase function integrates to 1 over the sphere, so a medium
// that scatters light of irradiance E, per unit scattering, shows E * phase:
// no further factors of PI. The irradiance map holds the cosine-weighted mean
// radiance of a hemisphere (what a white Lambert surface shows under it), so
// an isotropic medium in a uniform sky shows the map's value as it stands.

@group(0) @binding(80) var<uniform> gas: GasUniforms;
// Density, temperature, fuel and flame per cell.
@group(0) @binding(81) var gas_state: texture_3d<f32>;
// Velocity in metres per second (xyz).
@group(0) @binding(82) var gas_velocity: texture_3d<f32>;
// Linear, clamped to the box.
@group(0) @binding(83) var gas_sampler: sampler;
// x: optical depth toward the sun to the box's far face, y: straight up,
// z: the flames blurred, for the glow (0 when off).
@group(0) @binding(84) var gas_light: texture_3d<f32>;
// Planck radiator colours: rgb at luminance 1, w luminance relative to 1500 K.
@group(0) @binding(85) var<storage, read> blackbody_table: array<vec4<f32>>;
@group(0) @binding(86) var opaque_depth: texture_depth_2d;
// Gas pressed against a solid looks the sun's shadow map up this many cells
// toward the sun: on the solid's own surface the depth test flickers between
// lit and shadowed and stripes the gas (shadow acne), and the shadow map's
// depth bias is sized for surfaces, not for a point in the air beside one.
// The gas's cells are half its air's (node.gas_volume), so this is two air
// cells.
const SHADOW_LIFT_CELLS: f32 = 4.0;
// Tileable gradient noise, one octave, 0 to 1.
@group(0) @binding(87) var detail_noise_tex: texture_3d<f32>;
@group(0) @binding(88) var detail_sampler: sampler;

struct GasUniforms {
    inv_view_proj: mat4x4<f32>,
    box_min_cell: vec4<f32>,      // xyz box min corner, w cell size
    box_max_seconds: vec4<f32>,   // xyz box max corner, w seconds
    smoke_density: vec4<f32>,     // rgb smoke colour, w extinction per metre at density 1
    look: vec4<f32>,              // x albedo, y detail, z flame brightness, w glow
    flame_kelvin: vec4<f32>,      // x kelvin at temperature 0, y at 1, z scatter octaves, w samples per cell
    sun_dir: vec4<f32>,           // xyz unit direction toward the sun, w its shadow caster slot (-1 none)
    sun_color: vec4<f32>,         // rgb premultiplied colour, w 1 when a sun lights the gas
    counts: vec4<u32>,            // x detail octaves, y 1 for scene shadows, z frame, w unused
    target_size: vec4<f32>,       // x width, y height, zw 0
};

// Safety cap so a broken box cannot hang the GPU; the step stretches past it
// to still cover the whole span.
const GAS_MAX_STEPS: u32 = 1024u;
// The march stops once less than this share of the light gets through.
const GAS_STOP_TRANSMITTANCE: f32 = 0.004;
// Corners at or nearer than this clip w are at the camera plane, where the
// box's screen rectangle stops being bounded.
const GAS_MIN_CLIP_W: f32 = 1e-4;
// The screen rectangle's margin round the box, in pixels.
const GAS_RECT_MARGIN_PX: f32 = 2.0;

// Detail noise: one lattice step of the first octave is this many gas cells,
// and the flow carries it for this long before a phase resets.
const DETAIL_CELLS: f32 = 4.0;
const DETAIL_FLOW_SECONDS: f32 = 0.8;
// Density swing of the detail at strength 1, and the flame's share of it.
const DETAIL_DENSITY_SWING: f32 = 1.5;
const DETAIL_FLAME_SHARE: f32 = 0.5;

// Phase: a forward Henyey-Greenstein lobe and a back one. Each further
// scattering octave halves the forward lobe's anisotropy.
const PHASE_FORWARD_G: f32 = 0.6;
const PHASE_BACK_G: f32 = -0.3;
const PHASE_FORWARD_WEIGHT: f32 = 0.8;
const SCATTER_OCTAVES_MAX: u32 = 4u;

// The sky's share of the ambient light a sample sees, and how fast the
// optical depth above it dims the sky and the ground.
const AMBIENT_SKY_SHARE: f32 = 0.5;
const AMBIENT_SKY_DIMMING: f32 = 0.5;
const AMBIENT_GROUND_DIMMING: f32 = 0.25;
// The flame's colour temperature on the smoke, as a share of the hottest.
const GLOW_KELVIN_SHARE: f32 = 0.85;

fn gas_unproject(ndc: vec2<f32>, raw_depth: f32) -> vec3<f32> {
    let w = gas.inv_view_proj * vec4<f32>(ndc, raw_depth, 1.0);
    return w.xyz / w.w;
}

// A screen-space rectangle round the box, drawn as two triangles at depth 0,
// so only the pixels the box can cover run the march. A corner at the camera
// plane makes it the whole screen.
@vertex
fn vs_gas(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let lo = gas.box_min_cell.xyz;
    let hi = gas.box_max_seconds.xyz;
    var ndc_min = vec2<f32>(1.0);
    var ndc_max = vec2<f32>(-1.0);
    var whole_screen = false;
    for (var c = 0u; c < 8u; c++) {
        let corner = select(lo, hi, vec3<bool>((c & 1u) != 0u, (c & 2u) != 0u, (c & 4u) != 0u));
        let clip = u.view_proj * vec4<f32>(corner, 1.0);
        if clip.w <= GAS_MIN_CLIP_W {
            whole_screen = true;
        } else {
            let ndc = clip.xy / clip.w;
            ndc_min = min(ndc_min, ndc);
            ndc_max = max(ndc_max, ndc);
        }
    }
    // A pixel is 2 / size in NDC.
    let margin = vec2<f32>(GAS_RECT_MARGIN_PX) * 2.0 / gas.target_size.xy;
    var rect_min = clamp(ndc_min - margin, vec2<f32>(-1.0), vec2<f32>(1.0));
    var rect_max = clamp(ndc_max + margin, vec2<f32>(-1.0), vec2<f32>(1.0));
    if whole_screen {
        rect_min = vec2<f32>(-1.0);
        rect_max = vec2<f32>(1.0);
    }
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    return vec4<f32>(mix(rect_min, rect_max, corners[vi]), 0.0, 1.0);
}

// Interleaved gradient noise (Jimenez 2014): a well spread value in [0, 1)
// per pixel, with no state and no texture read.
fn interleaved_gradient_noise(p: vec2<f32>) -> f32 {
    return fract(52.9829189 * fract(dot(p, vec2<f32>(0.06711056, 0.00583715))));
}

fn henyey_greenstein(cos_theta: f32, g: f32) -> f32 {
    let denominator = 1.0 + g * g - 2.0 * g * cos_theta;
    return (1.0 - g * g) / (4.0 * PI * pow(max(denominator, 1e-4), 1.5));
}

fn dual_lobe_phase(cos_theta: f32, g: f32) -> f32 {
    return PHASE_FORWARD_WEIGHT * henyey_greenstein(cos_theta, g)
        + (1.0 - PHASE_FORWARD_WEIGHT) * henyey_greenstein(cos_theta, PHASE_BACK_G);
}

// The light of a black body at `kelvin`, in units of a 1500 K body's
// luminance: the table's colour times its relative luminance, interpolated
// between entries and clamped to the table's range.
fn blackbody(kelvin: f32) -> vec3<f32> {
    let span = BLACKBODY_MAX_KELVIN - BLACKBODY_MIN_KELVIN;
    let position = clamp((kelvin - BLACKBODY_MIN_KELVIN) / span, 0.0, 1.0) * f32(BLACKBODY_ENTRIES - 1u);
    let lower = min(u32(position), BLACKBODY_ENTRIES - 2u);
    let entry = mix(blackbody_table[lower], blackbody_table[lower + 1u], position - f32(lower));
    return entry.rgb * entry.w;
}

// The map's cosine-weighted mean radiance round `dir`. Read from the map
// itself: a ray-traced irradiance substitution is per pixel of the surface
// behind the gas, not a property of the sky.
fn sky_radiance(dir: vec3<f32>) -> vec3<f32> {
    return textureSampleLevel(irradiance_map, envmap_sampler, env_lookup_uv(dir), 0.0).rgb;
}

fn octave_scale(i: u32) -> f32 {
    switch i {
        case 0u: { return 1.0; }
        case 1u: { return 2.03; }
        case 2u: { return 4.01; }
        default: { return 7.97; }
    }
}

// Octaves of the noise at `q`, in lattice steps, weighted 1, 1/2, 1/4... and
// scaled so the spread stays the same however many octaves run: 0.5 mean.
fn detail_fbm(q: vec3<f32>) -> f32 {
    let octaves = gas.counts.x;
    var sum = 0.0;
    var power = 0.0;
    var amplitude = 1.0;
    for (var i = 0u; i < octaves; i++) {
        let tex = textureSampleLevel(
            detail_noise_tex, detail_sampler, q * (octave_scale(i) / NOISE_LATTICE_CELLS), 0.0,
        ).x;
        sum += amplitude * (tex - 0.5);
        power += amplitude * amplitude;
        amplitude *= 0.5;
    }
    return 0.5 + sum * inverseSqrt(max(power, 1e-6));
}

// The detail noise at world point `p` in a flow of velocity `v`. The noise
// coordinates are carried by the flow for one period and then snap back; two
// phases half a period apart cross-fade, so each is at full weight when the
// other resets and the pattern moves with the smoke without stretching.
fn detail_at(p: vec3<f32>, v: vec3<f32>) -> f32 {
    let seconds = gas.box_max_seconds.w;
    let phase_a = fract(seconds / DETAIL_FLOW_SECONDS);
    let phase_b = fract(seconds / DETAIL_FLOW_SECONDS + 0.5);
    let weight_a = 1.0 - abs(2.0 * phase_a - 1.0);
    let lattice = DETAIL_CELLS * gas.box_min_cell.w;
    let q = p / lattice;
    let carried = v * (DETAIL_FLOW_SECONDS / lattice);
    return weight_a * detail_fbm(q - carried * phase_a)
        + (1.0 - weight_a) * detail_fbm(q - carried * phase_b);
}

// The share of the sun's light the scene's own shadow map lets through at
// `p`: one hardware comparison tap, lit outside the map.
fn scene_sun_visibility(p: vec3<f32>) -> f32 {
    if gas.sun_dir.w < 0.0 {
        return 1.0;
    }
    // The sun's cascade that holds `p`.
    let slot = caster_face(i32(gas.sun_dir.w + 0.5), p);
    // Texel 0 marks a caster with no shadow-map layer.
    if casters[slot].shadow.z <= 0.0 {
        return 1.0;
    }
    let clip = casters[slot].view_proj * vec4<f32>(p, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    // The mapping `project_to_shadow_uv` makes, with the depth beside it.
    let suv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    if any(suv < vec2<f32>(0.0)) || any(suv > vec2<f32>(1.0)) || ndc.z > 1.0 || ndc.z < 0.0 {
        return 1.0;
    }
    return sample_shadow(slot, suv, ndc.z - casters[slot].shadow.x);
}

@fragment
fn fs_gas(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = vec2<i32>(pos.xy);
    let uv = pos.xy / gas.target_size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let origin = gas_unproject(ndc, 1.0);
    let dir = normalize(gas_unproject(ndc, 0.5) - origin);

    // The box, then the nearest opaque surface.
    let box_lo = gas.box_min_cell.xyz;
    let box_hi = gas.box_max_seconds.xyz;
    let box_size = box_hi - box_lo;
    let tiny = abs(dir) < vec3<f32>(1e-9);
    let safe_dir = select(dir, vec3<f32>(1e-9), tiny);
    let ta = (box_lo - origin) / safe_dir;
    let tb = (box_hi - origin) / safe_dir;
    let t_near = min(ta, tb);
    let t_far = max(ta, tb);
    let t0 = max(max(max(t_near.x, t_near.y), t_near.z), 0.0);
    var t1 = min(min(t_far.x, t_far.y), t_far.z);
    let opaque = textureLoad(opaque_depth, pix, 0);
    if opaque > 0.0 {
        t1 = min(t1, dot(gas_unproject(ndc, opaque) - origin, dir));
    }
    if t0 >= t1 {
        discard;
    }

    let cell = gas.box_min_cell.w;
    let span = t1 - t0;
    let steps = clamp(u32(ceil(span * gas.flame_kelvin.w / cell)), 1u, GAS_MAX_STEPS);
    let ds = span / f32(steps);
    let jitter = interleaved_gradient_noise(pos.xy + vec2<f32>(5.588238 * f32(gas.counts.z % 64u)));

    let albedo = gas.look.x;
    let detail = gas.look.y;
    let flame_brightness = gas.look.z;
    let glow = gas.look.w;
    let density_per_metre = gas.smoke_density.w;
    let scatter_color = gas.smoke_density.rgb * albedo;
    let scatter_octaves = min(u32(gas.flame_kelvin.z), SCATTER_OCTAVES_MAX);
    let has_sun = gas.sun_color.w > 0.5;
    let sun_dir = gas.sun_dir.xyz;
    let cos_theta = dot(dir, sun_dir);
    // The solid's shadow maps stand in for ray-traced shadows, which are per
    // pixel of the surface behind the gas.
    let shadow_maps_live = !(u.scene_params.w > 0.5 && u.rt_flags.w > 0.5);
    let scene_shadows = has_sun && gas.counts.y != 0u && shadow_maps_live && gas.sun_dir.w >= 0.0;

    // Per scattering octave i: the sun's light dimmed by exp(-b^i tau) with
    // weight a^i, seen through a phase of anisotropy c^i g; a = b = c = 1/2
    // (Wrenninge 2015). The phase does not change along the ray.
    var octave_phase = array<f32, 4>(0.0, 0.0, 0.0, 0.0);
    // How much an albedo-1 isotropic medium deep in the sun's light shows of
    // the sum, against what a white Lambert surface lit the same way shows.
    // Dividing it out makes such a medium as bright as that surface, whatever
    // the octave count.
    var deep_medium = 0.0;
    var octave_weight = 1.0;
    for (var i = 0u; i < scatter_octaves; i++) {
        octave_phase[i] = dual_lobe_phase(cos_theta, octave_weight * PHASE_FORWARD_G);
        deep_medium += octave_weight / (1.0 + octave_weight);
        octave_weight *= 0.5;
    }
    // Lambert shows E / PI, the isotropic medium E / (4 PI) per unit of the sum.
    let scatter_gain = 4.0 / max(deep_medium, 1e-6);

    // The sky above and below, from the irradiance map: the hemisphere's
    // mean radiance is taken as the mean of the map towards its pole and
    // round its rim. An isotropic medium sees both hemispheres equally.
    let rim = 0.25 * (
        sky_radiance(vec3<f32>(1.0, 0.0, 0.0)) + sky_radiance(vec3<f32>(-1.0, 0.0, 0.0))
        + sky_radiance(vec3<f32>(0.0, 0.0, 1.0)) + sky_radiance(vec3<f32>(0.0, 0.0, -1.0))
    );
    let sky = 0.5 * (sky_radiance(vec3<f32>(0.0, 1.0, 0.0)) + rim);
    let ground = 0.5 * (sky_radiance(vec3<f32>(0.0, -1.0, 0.0)) + rim);
    let glow_color = blackbody(gas.flame_kelvin.y * GLOW_KELVIN_SHARE) * (glow * flame_brightness);

    var color = vec3<f32>(0.0);
    var transmittance = 1.0;
    // The light-weighted mean distance, for the fog.
    var weighted_distance = 0.0;
    var weight_sum = 0.0;
    for (var i = 0u; i < steps; i++) {
        let t = t0 + (f32(i) + jitter) * ds;
        let p = origin + dir * t;
        let uvw = (p - box_lo) / box_size;
        let cell_state = textureSampleLevel(gas_state, gas_sampler, uvw, 0.0);
        var density = cell_state.x;
        var flame = cell_state.w;
        if density <= 1e-4 && flame <= 1e-4 {
            continue;
        }

        var flame_gain = 1.0;
        if detail > 0.0 {
            let flow = textureSampleLevel(gas_velocity, gas_sampler, uvw, 0.0).xyz;
            let swing = detail * DETAIL_DENSITY_SWING * (2.0 * detail_at(p, flow) - 1.0);
            density *= clamp(1.0 + swing, 0.0, 2.0);
            flame_gain = clamp(1.0 + DETAIL_FLAME_SHARE * swing, 0.0, 2.0);
            flame *= flame_gain;
        }

        let sigma_t = density * density_per_metre;
        let step_transmittance = exp(-sigma_t * ds);
        var added = vec3<f32>(0.0);

        if sigma_t > 1e-4 {
            let optical = textureSampleLevel(gas_light, gas_sampler, uvw, 0.0).xyz;
            var incoming = vec3<f32>(0.0);
            if has_sun {
                var visibility = 1.0;
                if scene_shadows {
                    visibility = scene_sun_visibility(p + sun_dir * (SHADOW_LIFT_CELLS * cell));
                }
                var falloff = exp(-optical.x);
                var weight = 1.0;
                var multiple = 0.0;
                for (var o = 0u; o < scatter_octaves; o++) {
                    multiple += weight * falloff * octave_phase[o];
                    weight *= 0.5;
                    falloff = sqrt(falloff);
                }
                incoming += gas.sun_color.rgb * (visibility * multiple * scatter_gain);
            }
            incoming += AMBIENT_SKY_SHARE * (
                sky * exp(-AMBIENT_SKY_DIMMING * optical.y)
                + ground * exp(-AMBIENT_GROUND_DIMMING * optical.y)
            );
            incoming += glow_color * optical.z;
            added += scatter_color * incoming * (1.0 - step_transmittance);
        }

        // Flames are thin and bright: their light is the mean of this sample
        // and one half a step on, since at one sample a step their fronts
        // alias into a crosshatch of the per-pixel jitter.
        if flame > 1e-4 {
            let on = textureSampleLevel(gas_state, gas_sampler, uvw + dir * (0.5 * ds) / box_size, 0.0);
            let kelvin = mix(gas.flame_kelvin.x, gas.flame_kelvin.y, saturate(cell_state.y));
            let kelvin_on = mix(gas.flame_kelvin.x, gas.flame_kelvin.y, saturate(on.y));
            let light = blackbody(kelvin) * flame + blackbody(kelvin_on) * (on.w * flame_gain);
            added += light * (0.5 * flame_brightness * ds);
        }

        let contribution = transmittance * added;
        color += contribution;
        let luminance = dot(contribution, vec3<f32>(0.2126, 0.7152, 0.0722));
        weighted_distance += luminance * t;
        weight_sum += luminance;
        transmittance *= step_transmittance;
        if transmittance < GAS_STOP_TRANSMITTANCE {
            break;
        }
    }

    let alpha = 1.0 - transmittance;
    if alpha <= 1e-5 && all(color <= vec3<f32>(1e-6)) {
        discard;
    }
    // Fog covers only the share of the pixel the gas covers, as the water's
    // mist does, at the distance its light comes from.
    if u.fog_params.x > 0.0 {
        let mean_t = select(0.5 * (t0 + t1), weighted_distance / weight_sum, weight_sum > 1e-12);
        let fog = fog_amount(origin + dir * mean_t);
        color = color * (1.0 - fog) + u.fog_color.rgb * (fog * alpha);
    }
    return vec4<f32>(color * exp2(u.scene_params.z), alpha);
}
