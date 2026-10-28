// Caustics: the sun's light focused by the water onto what lies under it, by
// caustic mapping (Shah, Konttinen and Pattanaik 2007, "Caustics mapping: an
// image-space technique for real-time caustics").
//
// The caustic map is the sun's view narrowed to the water's bounds (`cs_fit`),
// so its texels are millimetres on a tank, not the sun's shadow texels across
// its whole range. The water's surface is drawn into `surface` in that view
// (`vs_surface`); each texel of it is a photon (`cs_splat`): it refracts the
// sun's light through the surface there, finds where the refracted ray meets
// the opaque scene in the sun's shadow map, and adds the light that gets
// through (Fresnel and the water's absorption over the path) to the caustic
// map at that point. The resolve blurs the sums a little and caps them; scene
// shading reads the map for whatever the sun sees through the water
// (`caustic_light` in `scene_shading.wgsl`).
//
// Each photon's normal is the surface's slope across about one display cell,
// so the mesh's facets do not print into the light, tilted by the same ripple
// detail the water shade lights the surface with (`look_ripple_detail.wgsl`,
// composed before this file with the field's table reads and the foam cover)
// at the size of one texel: the small ripples are what focus sunlight into
// lines.
//
// A flat still pool sends each texel's photon to one texel, so the map reads
// T * exp(-sigma * depth) under it: the floor is dimmed by the water, not
// brightened. A curved surface bunches photons into lines where it focuses.

struct Splat {
    // The sun's view-projection, which its shadow map is drawn with, and its
    // inverse.
    sun_view_proj: mat4x4<f32>,
    inverse_sun_view_proj: mat4x4<f32>,
    // The water's object space to world.
    model: mat4x4<f32>,
    // xyz: unit direction toward the sun; w: the water's index of refraction.
    toward_sun: vec4<f32>,
    // rgb: the water's extinction per metre; w: the sun's shadow-map texel in
    // uv (1 / its resolution).
    extinction: vec4<f32>,
    // x: the caustic map's side; y: the sun's shadow-map layer; z: the sun's
    // caster slot + 1; w 0.
    sizes: vec4<u32>,
    // x: how far past the water's surface, in the sun's depth, a receiver
    // must lie to be under it; y: how strongly the caustics show; zw 0.
    bias: vec4<f32>,
};

// The caustic map's view; field for field with `CausticFit` in
// `scene_shading.wgsl`.
struct CausticFit {
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    // The sun's caster slot + 1; 0 when there is no water to light.
    slot: u32,
    bias: f32,
    intensity: f32,
    pad: u32,
};

struct MeshVertex {
    position: vec3<f32>, _pad0: f32,
    normal: vec3<f32>, _pad1: f32,
    uv: vec2<f32>, _pad2: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> s: Splat;
@group(0) @binding(1) var surface: texture_depth_2d;
@group(0) @binding(2) var shadow_maps: texture_depth_2d_array;
@group(0) @binding(3) var<storage, read_write> sums: array<atomic<u32>>;
@group(0) @binding(4) var caustic_out: texture_storage_2d<rgba16float, write>;
// `cs_fit` writes the view at 5; every pass after it reads the same buffer
// at 6.
@group(0) @binding(5) var<storage, read_write> fit_out: CausticFit;
@group(0) @binding(6) var<storage, read> fit: CausticFit;
@group(0) @binding(7) var<storage, read> vertices: array<MeshVertex>;

// Light per photon in the sums' fixed point.
const CA_FIXED: f32 = 65536.0;
// Water's reflectance head on, ((n - 1) / (n + 1))^2 at n = 1.333.
const CA_F0: f32 = 0.02;
// The most texels either side the surface's slope is taken across.
const CA_MAX_SPAN: i32 = 16;
// Refinements of where the refracted ray meets the scene.
const CA_STEPS: u32 = 2u;
// The brightest a focus reads, times the sun's own light, so a sharp focus
// stays a bright line and never a sparkle.
const CA_MAX: f32 = 4.0;
// The narrowed view's margin round the water, as a share of its size.
const CA_FIT_MARGIN: f32 = 0.05;

// The sun's view narrowed to a square round the water's bounds, within the
// sun's own view.
@compute @workgroup_size(1)
fn cs_fit() {
    fit_out.slot = 0u;
    fit_out.bias = s.bias.x;
    fit_out.intensity = s.bias.y;
    if field_extent_empty() {
        return;
    }
    let lo = field_extent_min();
    let hi = field_extent_max();
    var low = vec2<f32>(1.0e30);
    var high = vec2<f32>(-1.0e30);
    for (var corner = 0u; corner < 8u; corner = corner + 1u) {
        let pick = vec3<bool>((corner & 1u) != 0u, (corner & 2u) != 0u, (corner & 4u) != 0u);
        let clip = s.sun_view_proj * (s.model * vec4<f32>(select(lo, hi, pick), 1.0));
        let xy = clip.xy / clip.w;
        low = min(low, xy);
        high = max(high, xy);
    }
    let centre = 0.5 * (low + high);
    let half = 0.5 * max(high.x - low.x, high.y - low.y) * (1.0 + CA_FIT_MARGIN);
    low = max(centre - vec2<f32>(half), vec2<f32>(-1.0));
    high = min(centre + vec2<f32>(half), vec2<f32>(1.0));
    if any(low >= high) {
        return;
    }
    // Clip x and y from [low, high] onto [-1, 1]; depth stays the sun's.
    let scale = 2.0 / (high - low);
    let offset = -(high + low) / (high - low);
    let narrow = mat4x4<f32>(
        vec4<f32>(scale.x, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, scale.y, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(offset.x, offset.y, 0.0, 1.0),
    );
    let widen = mat4x4<f32>(
        vec4<f32>(1.0 / scale.x, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, 1.0 / scale.y, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(-offset.x / scale.x, -offset.y / scale.y, 0.0, 1.0),
    );
    fit_out.view_proj = narrow * s.sun_view_proj;
    fit_out.inverse_view_proj = s.inverse_sun_view_proj * widen;
    fit_out.slot = s.sizes.z;
}

// The water's surface mesh in the caustic map's view, depth only.
@vertex
fn vs_surface(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    return fit.view_proj * (s.model * vec4<f32>(vertices[vid].position, 1.0));
}

@fragment
fn fs_surface() {}

fn ca_uv(ndc: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
}

fn ca_ndc(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
}

// The opaque point the sun sees along its ray through x, from its shadow map
// (which never holds the water); w is 0 off the map or where nothing is.
fn ca_receiver(x: vec3<f32>) -> vec4<f32> {
    let c = s.sun_view_proj * vec4<f32>(x, 1.0);
    let uv = ca_uv(c.xy / c.w);
    if any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) {
        return vec4<f32>(0.0);
    }
    // The sun renders into the top-left (1 / texel)^2 of its layer.
    let depth = textureLoad(shadow_maps, vec2<i32>(uv / s.extinction.w), i32(s.sizes.y), 0);
    if depth >= 1.0 {
        return vec4<f32>(0.0);
    }
    let h = s.inverse_sun_view_proj * vec4<f32>(ca_ndc(uv), depth, 1.0);
    return vec4<f32>(h.xyz / h.w, 1.0);
}

// The water's surface at texel `px` of the caustic map; w is 0 off the map
// or where there is no water.
fn ca_surface(px: vec2<i32>) -> vec4<f32> {
    let side = i32(s.sizes.x);
    if any(px < vec2<i32>(0)) || any(px >= vec2<i32>(side)) {
        return vec4<f32>(0.0);
    }
    let depth = textureLoad(surface, px, 0);
    if depth >= 1.0 {
        return vec4<f32>(0.0);
    }
    let uv = (vec2<f32>(px) + vec2<f32>(0.5)) / f32(side);
    let h = fit.inverse_view_proj * vec4<f32>(ca_ndc(uv), depth, 1.0);
    return vec4<f32>(h.xyz / h.w, 1.0);
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
    if fit.slot == 0u || any(px >= vec2<i32>(side)) {
        return;
    }
    let here = ca_surface(px);
    if here.w == 0.0 {
        return;
    }
    let p = here.xyz;
    // One texel in metres: 2 / side of clip space over the view's clip x per
    // metre. The slope spans about one display cell.
    let row_x = vec3<f32>(fit.view_proj[0].x, fit.view_proj[1].x, fit.view_proj[2].x);
    let texel_metres = 2.0 / (f32(side) * length(row_x));
    let span = clamp(i32(round(ripple.field.x / texel_metres)), 1, CA_MAX_SPAN);
    let along_x = ca_slope(ca_surface(px + vec2<i32>(span, 0)), ca_surface(px - vec2<i32>(span, 0)), p);
    let along_y = ca_slope(ca_surface(px + vec2<i32>(0, span)), ca_surface(px - vec2<i32>(0, span)), p);
    let across = cross(along_x.xyz, along_y.xyz);
    if along_x.w * along_y.w == 0.0 || !(dot(across, across) > 0.0) {
        return;
    }
    let sun = s.toward_sun.xyz;
    var n = normalize(across);
    if dot(n, sun) < 0.0 {
        n = -n;
    }
    // The ripples at one texel's size (`ripple.time.zw` is 1, 0).
    n = look_ripple_detail(p, n, texel_metres);
    let cos_in = dot(n, sun);
    if cos_in <= 0.0 {
        return;
    }
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
    let c = fit.view_proj * vec4<f32>(hit.xyz, 1.0);
    let at = ca_uv(c.xy / c.w) * f32(side) - vec2<f32>(0.5);
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
