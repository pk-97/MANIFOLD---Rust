// Look hook: wet surfaces. Called from `fs_pbr` after the material resolves,
// keyed on the pixel because materials are shared. A water film on a rough
// diffuse surface darkens it: light the surface scatters is caught under the
// film by total internal reflection and returns to the surface, which absorbs
// part of it each time (Lekner and Dorf 1988, "Why some things are darker when
// wet", Applied Optics 27(7)). With the water's index `n`, the diffuse
// (hemispherical) reflectance of the film's outer face `r_e`, the inner face's
// `r_i = 1 - (1 - r_e) / n^2` and the dry albedo `A`, the soaked albedo is
//   A_wet = (1 - r_e) (1 - r_i) A / (1 - r_i A),   per channel.
// A film is also optically smooth, so its roughness is water's: 0.
//
// The wetness comes from the scene's wet mask: the wet pass draws the water's
// wet marks (`node.wet_map`, `wet_pass.wgsl`) over the opaque depth before
// any colour, so this reads it at the pixel being shaded.
//
// Binds 68 itself; the water shade includes this file but never calls the
// hook, so the water does not bind it.

// How wet the surface is, 0 to 1, at 1 / WET_MASK_SCALE of the render's size
// (`wet_pass::MASK_SCALE`); the renderer's one black texel, which reads dry,
// when no water has a wet map and for glass, which the mask is not about.
@group(0) @binding(68) var wet_mask: texture_2d<f32>;
const WET_MASK_SCALE: f32 = 2.0;

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

// The wetness at pixel `frag_xy`, 0 to 1, bilinear between the mask's texels.
fn wetness_at(frag_xy: vec2<f32>) -> f32 {
    let last = vec2<i32>(textureDimensions(wet_mask)) - vec2<i32>(1);
    let p = frag_xy / WET_MASK_SCALE - vec2<f32>(0.5);
    let low = floor(p);
    let f = p - low;
    let a = clamp(vec2<i32>(low), vec2<i32>(0), last);
    let b = clamp(vec2<i32>(low) + vec2<i32>(1), vec2<i32>(0), last);
    let top = mix(textureLoad(wet_mask, a, 0).r, textureLoad(wet_mask, vec2<i32>(b.x, a.y), 0).r, f.x);
    let bottom = mix(textureLoad(wet_mask, vec2<i32>(a.x, b.y), 0).r, textureLoad(wet_mask, b, 0).r, f.x);
    return mix(top, bottom, f.y);
}

fn look_wet_surface(frag_xy: vec2<f32>, albedo: vec3<f32>, roughness: f32) -> WetSurface {
    let w = wetness_at(frag_xy);
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
