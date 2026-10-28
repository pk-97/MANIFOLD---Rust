// Caustics: the sun's light focused by the water onto what lies under it, by
// caustic mapping (Shah, Konttinen and Pattanaik 2007, "Caustics mapping: an
// image-space technique for real-time caustics"). The water's surface is drawn
// from the sun into `surface` (its depth in the sun's view); each texel of it
// is a photon: it refracts the sun's light through the surface there, finds
// where the refracted ray meets the opaque scene in the sun's shadow map, and
// adds the light that gets through (Fresnel and the water's absorption over
// the path) to that point of the caustic map, in the sun's view too. The
// resolve blurs the sums a little and caps them; scene shading reads the map
// for receivers the sun sees through the water (`caustic_light` in
// `scene_shading.wgsl`).
//
// A flat still pool sends each texel's photon to one texel, so the map reads
// T * exp(-sigma * depth) under it: the floor is dimmed by the water, not
// brightened. A curved surface bunches photons into lines where it focuses.

struct Splat {
    light_view_proj: mat4x4<f32>,
    inverse_light_view_proj: mat4x4<f32>,
    // xyz: unit direction toward the sun; w: the water's index of refraction.
    toward_sun: vec4<f32>,
    // rgb: the water's extinction per metre; w: the sun's shadow-map texel in
    // uv (1 / its resolution).
    extinction: vec4<f32>,
    // x: the caustic map's side; y: the sun's shadow-map layer; zw 0.
    sizes: vec4<u32>,
};

@group(0) @binding(0) var<uniform> s: Splat;
@group(0) @binding(1) var surface: texture_depth_2d;
@group(0) @binding(2) var shadow_maps: texture_depth_2d_array;
@group(0) @binding(3) var<storage, read_write> sums: array<atomic<u32>>;
@group(0) @binding(4) var caustic_out: texture_storage_2d<rgba16float, write>;

// Light per photon in the sums' fixed point.
const CA_FIXED: f32 = 65536.0;
// Water's reflectance head on, ((n - 1) / (n + 1))^2 at n = 1.333.
const CA_F0: f32 = 0.02;
// The surface normal comes from the surface this many texels either side, so
// the mesh's facets do not print into the light.
const CA_SPAN: i32 = 2;
// Refinements of where the refracted ray meets the scene.
const CA_STEPS: u32 = 2u;
// The brightest a focus reads, times the sun's own light, so a sharp focus
// stays a bright line and never a sparkle.
const CA_MAX: f32 = 4.0;

fn ca_unproject(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let h = s.inverse_light_view_proj * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, depth, 1.0);
    return h.xyz / h.w;
}

// Point x in the sun's view: uv and depth.
fn ca_project(x: vec3<f32>) -> vec3<f32> {
    let c = s.light_view_proj * vec4<f32>(x, 1.0);
    let n = c.xyz / c.w;
    return vec3<f32>(n.x * 0.5 + 0.5, n.y * -0.5 + 0.5, n.z);
}

// The opaque point the sun sees along its ray through x, from its shadow map
// (which never holds the water); w is 0 off the map or where nothing is.
fn ca_receiver(x: vec3<f32>) -> vec4<f32> {
    let p = ca_project(x);
    if any(p.xy < vec2<f32>(0.0)) || any(p.xy >= vec2<f32>(1.0)) {
        return vec4<f32>(0.0);
    }
    // The sun renders into the top-left (1 / texel)^2 of its layer.
    let texel = vec2<i32>(p.xy / s.extinction.w);
    let depth = textureLoad(shadow_maps, texel, i32(s.sizes.y), 0);
    if depth >= 1.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(ca_unproject(p.xy, depth), 1.0);
}

// The water's surface at texel `px`; w is 0 off the map or where there is no
// water.
fn ca_surface(px: vec2<i32>) -> vec4<f32> {
    let side = i32(s.sizes.x);
    if any(px < vec2<i32>(0)) || any(px >= vec2<i32>(side)) {
        return vec4<f32>(0.0);
    }
    let depth = textureLoad(surface, px, 0);
    if depth >= 1.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(ca_unproject((vec2<f32>(px) + vec2<f32>(0.5)) / f32(side), depth), 1.0);
}

// The surface's slope along one axis: across both neighbours when both are
// water, else to the one that is; w is 0 with neither.
fn ca_slope(ahead: vec4<f32>, behind: vec4<f32>, here: vec3<f32>) -> vec4<f32> {
    if ahead.w > 0.0 && behind.w > 0.0 {
        return vec4<f32>(ahead.xyz - behind.xyz, 1.0);
    }
    if ahead.w > 0.0 {
        return vec4<f32>(ahead.xyz - here, 1.0);
    }
    if behind.w > 0.0 {
        return vec4<f32>(here - behind.xyz, 1.0);
    }
    return vec4<f32>(0.0);
}

fn ca_add(px: vec2<i32>, side: i32, light: vec3<f32>) {
    if any(px < vec2<i32>(0)) || any(px >= vec2<i32>(side)) {
        return;
    }
    let i = 3u * u32(px.y * side + px.x);
    let fixed = vec3<u32>(light * CA_FIXED + vec3<f32>(0.5));
    atomicAdd(&sums[i], fixed.x);
    atomicAdd(&sums[i + 1u], fixed.y);
    atomicAdd(&sums[i + 2u], fixed.z);
}

@compute @workgroup_size(16, 16)
fn cs_splat(@builtin(global_invocation_id) gid: vec3<u32>) {
    let side = i32(s.sizes.x);
    let px = vec2<i32>(gid.xy);
    if any(px >= vec2<i32>(side)) {
        return;
    }
    let here = ca_surface(px);
    if here.w == 0.0 {
        return;
    }
    let p = here.xyz;
    let along_x = ca_slope(ca_surface(px + vec2<i32>(CA_SPAN, 0)), ca_surface(px - vec2<i32>(CA_SPAN, 0)), p);
    let along_y = ca_slope(ca_surface(px + vec2<i32>(0, CA_SPAN)), ca_surface(px - vec2<i32>(0, CA_SPAN)), p);
    let across = cross(along_x.xyz, along_y.xyz);
    if along_x.w * along_y.w == 0.0 || !(dot(across, across) > 0.0) {
        return;
    }
    let sun = s.toward_sun.xyz;
    var n = normalize(across);
    if dot(n, sun) < 0.0 {
        n = -n;
    }
    let cos_in = dot(n, sun);
    let through = 1.0 - (CA_F0 + (1.0 - CA_F0) * pow(1.0 - cos_in, 5.0));
    let d = refract(-sun, n, 1.0 / s.toward_sun.w);
    // Where the refracted ray meets the scene: first the point straight under
    // the surface, then the point under where the ray has gone that far.
    var hit = ca_receiver(p);
    if hit.w == 0.0 {
        return;
    }
    var t = distance(hit.xyz, p);
    for (var step = 0u; step < CA_STEPS; step = step + 1u) {
        hit = ca_receiver(p + d * t);
        if hit.w == 0.0 {
            return;
        }
        t = distance(hit.xyz, p);
    }
    let light = through * exp(-s.extinction.rgb * t);
    // Bilinear into the four texels round the hit.
    let at = ca_project(hit.xyz).xy * f32(side) - vec2<f32>(0.5);
    let low = floor(at);
    let f = at - low;
    let base = vec2<i32>(low);
    ca_add(base, side, light * (1.0 - f.x) * (1.0 - f.y));
    ca_add(base + vec2<i32>(1, 0), side, light * f.x * (1.0 - f.y));
    ca_add(base + vec2<i32>(0, 1), side, light * (1.0 - f.x) * f.y);
    ca_add(base + vec2<i32>(1, 1), side, light * f.x * f.y);
}

// The sums blurred by a 5 x 5 Gaussian of one texel (sigma 1), as light
// relative to the sun's, capped at CA_MAX.
@compute @workgroup_size(16, 16)
fn cs_resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    let side = i32(s.sizes.x);
    let px = vec2<i32>(gid.xy);
    if any(px >= vec2<i32>(side)) {
        return;
    }
    var weights = array<f32, 5>(0.135335, 0.606531, 1.0, 0.606531, 0.135335);
    var light = vec3<f32>(0.0);
    var total = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let q = px + vec2<i32>(dx, dy);
            if any(q < vec2<i32>(0)) || any(q >= vec2<i32>(side)) {
                continue;
            }
            let w = weights[dx + 2] * weights[dy + 2];
            let i = 3u * u32(q.y * side + q.x);
            light = light + w * vec3<f32>(
                f32(atomicLoad(&sums[i])),
                f32(atomicLoad(&sums[i + 1u])),
                f32(atomicLoad(&sums[i + 2u])),
            );
            total = total + w;
        }
    }
    let c = min(light / (total * CA_FIXED), vec3<f32>(CA_MAX));
    textureStore(caustic_out, px, vec4<f32>(c, 1.0));
}
