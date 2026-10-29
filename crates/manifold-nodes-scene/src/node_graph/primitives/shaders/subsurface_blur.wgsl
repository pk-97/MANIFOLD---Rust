// node.render_scene subsurface blur: spreads each scattering surface's
// diffuse light by Burley's normalized diffusion profile (Christensen and
// Burley 2015), sampled on screen as Unity HDRP does (Golubev 2018). For a
// surface of profile distance d, per colour, the light leaving at distance r
// from where it entered is
//   R(r) = (e^(-r/d) + e^(-r/(3d))) / (8 π d r),
// which sums to 1 over the plane. A pixel draws its samples at radii from
// the profile of its largest d (`radii`, stratified, in units of d) along a
// golden-angle spiral turned per pixel. Each sample weighs the profile at its
// true distance over the density it was drawn with, per colour, and the sum
// divides by the weights, so a flat evenly lit surface keeps its exact light
// and light lost off an edge comes back. A sample on another surface or the
// background counts nothing, and one across a depth step lies far away in 3D
// and counts little.

struct BlurParams {
    inv_view_proj: mat4x4<f32>,
    view_proj: mat4x4<f32>,
    size: vec2<u32>,
    // Pixels per metre at view depth 1.
    focal: f32,
    _pad0: u32,
    // SAMPLES radii in units of d, four to a vector.
    radii: array<vec4<f32>, 8>,
}

struct Profiles {
    // Per scattering slot (slot - 1): d for red, green and blue, the largest
    // in w.
    distance: array<vec4<f32>, 64>,
}

const SAMPLES: u32 = 32u;
const GOLDEN_ANGLE: f32 = 2.39996323;

@group(0) @binding(0) var<uniform> params: BlurParams;
@group(0) @binding(1) var<uniform> profiles: Profiles;
// Diffuse light before the albedo, the slot in alpha (0: no scattering).
@group(0) @binding(2) var light: texture_2d<f32>;
@group(0) @binding(3) var light_depth: texture_depth_2d;
@group(0) @binding(4) var scattered: texture_storage_2d<rgba16float, write>;

fn world_at(pixel: vec2<i32>, z: f32) -> vec3<f32> {
    let uv = (vec2<f32>(pixel) + vec2<f32>(0.5)) / vec2<f32>(params.size);
    let clip = params.inv_view_proj * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return clip.xyz / clip.w;
}

// Burley's profile over r without its constant 1 / (8 π d), which the
// per-colour normalisation cancels.
fn profile(r: f32, d: vec3<f32>) -> vec3<f32> {
    return (exp(-r / d) + exp(-r / (3.0 * d))) / (d * r);
}

@compute @workgroup_size(8, 8)
fn blur_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if any(gid.xy >= params.size) {
        return;
    }
    let pixel = vec2<i32>(gid.xy);
    let center = textureLoad(light, pixel, 0);
    let slot = u32(center.a + 0.5);
    if slot == 0u || slot > 64u {
        textureStore(scattered, pixel, center);
        return;
    }
    let d = profiles.distance[slot - 1u];
    let here = world_at(pixel, textureLoad(light_depth, pixel, 0));
    let view_depth = (params.view_proj * vec4<f32>(here, 1.0)).w;
    let pixels_per_metre = params.focal / max(view_depth, 1e-6);
    // The whole profile inside this pixel: nothing to spread.
    if d.w * params.radii[7].w * pixels_per_metre < 0.5 {
        textureStore(scattered, pixel, center);
        return;
    }
    // Interleaved gradient noise (Jimenez 2014) turns each pixel's spiral.
    let turn = 6.2831853 * fract(52.9829189 * fract(dot(vec2<f32>(pixel), vec2<f32>(0.06711056, 0.00583715))));
    var sum = vec3<f32>(0.0);
    var weight = vec3<f32>(0.0);
    for (var i = 0u; i < SAMPLES; i = i + 1u) {
        let r = params.radii[i / 4u][i % 4u] * d.w;
        let angle = turn + f32(i) * GOLDEN_ANGLE;
        let offset = r * pixels_per_metre * vec2<f32>(cos(angle), sin(angle));
        let q = pixel + vec2<i32>(round(offset));
        if any(q < vec2<i32>(0)) || any(q >= vec2<i32>(params.size)) {
            continue;
        }
        let other = textureLoad(light, q, 0);
        if u32(other.a + 0.5) != slot {
            continue;
        }
        let there = world_at(q, textureLoad(light_depth, q, 0));
        let w = profile(max(distance(there, here), r), d.xyz) / profile(r, vec3<f32>(d.w)).x;
        sum += w * other.rgb;
        weight += w;
    }
    var spread = center.rgb;
    if all(weight > vec3<f32>(0.0)) {
        spread = sum / weight;
    }
    textureStore(scattered, pixel, vec4<f32>(spread, center.a));
}
