// Look hook: wet surfaces. Called from `fs_pbr` after the material resolves,
// keyed on world position because materials are shared. A water film on a rough
// diffuse surface darkens it: light the surface scatters is caught under the
// film by total internal reflection and returns to the surface, which absorbs
// part of it each time (Lekner and Dorf 1988, "Why some things are darker when
// wet", Applied Optics 27(7)). With the water's index `n`, the diffuse
// (hemispherical) reflectance of the film's outer face `r_e`, the inner face's
// `r_i = 1 - (1 - r_e) / n^2` and the dry albedo `A`, the soaked albedo is
//   A_wet = (1 - r_e) (1 - r_i) A / (1 - r_i A),   per channel.
// A film is also optically smooth, so its roughness is water's: 0.
//
// The wetness comes from the water's wet map (`node.wet_map`, layout in
// `wet_map_layout.wgsl`), a top-down map over the water's object space of
// where and when the water touched a surface, read by world position: a point
// outside the map, or outside the heights its texel was touched at, is dry,
// and a touched one dries by the age of the last touch.
//
// Binds 68-69 itself; the water shade includes this file but never calls the
// hook, so the water binds neither of them.

struct WetUniforms {
    world_to_local: mat4x4<f32>,
    bound: vec4<u32>,  // x = 1 if a wet map is bound
};

@group(0) @binding(68) var<uniform> wet: WetUniforms;
@group(0) @binding(69) var<storage, read> wet_map: array<u32>;

struct WetSurface {
    albedo: vec3<f32>,
    roughness: f32,
}

// Lekner and Dorf 1988: water's refractive index and the diffuse reflectance of
// its outer face.
const WATER_IOR: f32 = 1.333;
const WATER_DIFFUSE_REFLECTANCE: f32 = 0.066;
// The inner face's diffuse reflectance, from the light that total internal
// reflection keeps: r_i = 1 - (1 - r_e) / n^2.
const WATER_INNER_REFLECTANCE: f32 = 1.0 - (1.0 - WATER_DIFFUSE_REFLECTANCE) / (WATER_IOR * WATER_IOR);

// The smallest roughness `resolve_mr` hands the GGX terms, which divide by it.
const WET_MIN_ROUGHNESS: f32 = 0.01;

// One texel's wetness at height y and whether it counts: a texel off the
// map, never touched, or touched only at other heights does not.
fn wet_texel(t: vec2<i32>, side: u32, y: f32, now: f32, drying: f32, margin: f32) -> vec2<f32> {
    if any(t < vec2<i32>(0)) || any(t >= vec2<i32>(i32(side))) {
        return vec2<f32>(0.0);
    }
    let word = wm_texel_word(vec2<u32>(t), side);
    let touch = wet_map[word];
    let low = wet_map[word + 1u];
    let high = wet_map[word + 2u];
    let bins = wet_map[word + 3u];
    if !wm_counts(touch, low, high, bins, y, margin) {
        return vec2<f32>(0.0);
    }
    return vec2<f32>(wm_wetness(touch, now, drying), 1.0);
}

// The wetness at a world position, 0 to 1: the four texels round it, each
// weighted bilinearly, over the ones that count. A wall's face lies on the
// texels its water stamped and the ones inside it, which never are, so only
// the stamped ones speak for it. 0 with no map bound, before the map is
// placed, off it, or where no texel round it counts.
fn wetness_at(world_pos: vec3<f32>) -> f32 {
    if wet.bound.x == 0u {
        return 0.0;
    }
    let texel = bitcast<f32>(wet_map[WM_TEXEL]);
    if !(texel > 0.0) {
        return 0.0;
    }
    let side = wet_map[WM_SIDE];
    let local = (wet.world_to_local * vec4<f32>(world_pos, 1.0)).xyz;
    let origin = vec2<f32>(bitcast<f32>(wet_map[WM_ORIGIN_X]), bitcast<f32>(wet_map[WM_ORIGIN_Z]));
    // Texel centres sit half a texel in from their low corner.
    let uv = (local.xz - origin) / texel - vec2<f32>(0.5);
    if any(uv < vec2<f32>(-1.0)) || any(uv > vec2<f32>(f32(side))) {
        return 0.0;
    }
    let low = floor(uv);
    let f = uv - low;
    let t = vec2<i32>(low);
    let now = bitcast<f32>(wet_map[WM_NOW]);
    let drying = bitcast<f32>(wet_map[WM_DRYING]);
    let margin = WM_HEIGHT_MARGIN_TEXELS * texel;
    let y = local.y;
    let sum = (1.0 - f.x) * (1.0 - f.y) * wet_texel(t, side, y, now, drying, margin)
        + f.x * (1.0 - f.y) * wet_texel(t + vec2<i32>(1, 0), side, y, now, drying, margin)
        + (1.0 - f.x) * f.y * wet_texel(t + vec2<i32>(0, 1), side, y, now, drying, margin)
        + f.x * f.y * wet_texel(t + vec2<i32>(1, 1), side, y, now, drying, margin);
    if !(sum.y > 0.0) {
        return 0.0;
    }
    return clamp(sum.x / sum.y, 0.0, 1.0);
}

fn look_wet_surface(world_pos: vec3<f32>, albedo: vec3<f32>, roughness: f32) -> WetSurface {
    let w = wetness_at(world_pos);
    if w <= 0.0 {
        return WetSurface(albedo, roughness);
    }
    let r_i = WATER_INNER_REFLECTANCE;
    let soaked = (1.0 - WATER_DIFFUSE_REFLECTANCE) * (1.0 - r_i) * albedo / (vec3<f32>(1.0) - r_i * albedo);
    return WetSurface(
        mix(albedo, soaked, w),
        max(mix(roughness, 0.0, w), WET_MIN_ROUGHNESS),
    );
}
