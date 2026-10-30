// Caustics: a light's light bent by the clear surfaces (water, and clear
// meshes such as goo, jelly and glass) onto what lies under them, by caustic
// mapping (Shah, Konttinen and Pattanaik 2007, "Caustics mapping: an
// image-space technique for real-time caustics"), with the light leaving
// through the far side as in Wyman 2005 ("An approximate image-space approach
// for interactive refraction").
//
// Each run is one light's view: a sun's or a spot's shadow view, or for a
// point light one cone toward the clear surfaces, across its cube faces. Its
// caustic map is that view narrowed to the clear surfaces' bounds (`cs_bounds`, `cs_fit`), so its texels are millimetres on
// a tank, not the light's shadow texels across its whole range. Every clear
// surface draws its side toward the light into `front`, which also tells
// shading what lies under one, and a clear mesh its far side into `back`,
// each with its normals and which surface it is (`vs_surface`, `fs_normal`).
// Each texel of `front` is a photon (`cs_splat`): it refracts the light in
// there (parallel from a sun, from its position for a point or spot),
// crosses the medium, refracts out where it meets the far side if that comes
// before the opaque scene, finds where it meets the opaque scene in the
// light's shadow map, and adds the light that gets through (Fresnel at each
// side, and the medium's absorption over its path inside) to the caustic map
// at that point. The resolve blurs the sums a little and caps them; scene
// shading reads the map for whatever the light sees through a clear surface
// (`caustic_light` in `scene_shading.wgsl`). The shadow maps hold no clear
// surface, so the map is their shadow too.
//
// A water's photon normal is its surface's slope across about one display
// cell, so the mesh's facets do not print into the light, tilted by the same
// ripple detail the water shade lights the surface with
// (`look_ripple_detail.wgsl`, composed before this file with the field's table
// reads) at the size of one texel: the small ripples are what focus light
// into lines. A clear mesh's photon normal is its own, as it shades with it.
//
// A flat still pool sends each texel's photon to one texel, so the map reads
// T * exp(-sigma * depth) under it: the floor is dimmed by the water, not
// brightened. A curved surface bunches photons into lines where it focuses.

struct Splat {
    // The light's shadow view-projection, which its shadow map is drawn
    // with, and its inverse.
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    // w 0: xyz is the unit direction toward a sun; w 1: a spot light's
    // position; w 2: a point light's, whose shadow views are its cube faces.
    light: vec4<f32>,
    // x: the caustic map's side; y: the light's shadow-map layer, its caster
    // slot; z: that slot + 1; w: this view's layer of the caustic map.
    sizes: vec4<u32>,
    // x: how far past a clear surface, in metres along the light, a receiver
    // must lie to be under it; y: how strongly the caustics show; z: the
    // light's shadow-map texel in uv (1 / its resolution); w 0.
    bias: vec4<f32>,
    // The views the light's shadow maps are drawn with, from layer y on, and
    // their inverses: a point light's six cube faces, else the light's own
    // view first.
    shadow_views: array<mat4x4<f32>, 6>,
    inverse_shadow_views: array<mat4x4<f32>, 6>,
};

// One clear surface; field for field with `Clear` in `caustics.rs`.
struct Clear {
    // Its object space to world.
    model: mat4x4<f32>,
    // rgb: its medium's extinction per metre; w: its index of refraction, 1
    // for a thin wall, which does not bend the light.
    medium: vec4<f32>,
    // rgb: the share of the light it lets through (its transmission times
    // its base colour); w: 0 for a clear mesh, whose normals are its own and
    // whose light leaves through its far side; 1 for the water whose normals
    // are its slope and ripples, 2 for any other water, by its slope alone. A
    // water's light ends on what lies under it.
    through: vec4<f32>,
    // x: 1 when `counts` holds its live count; y: the count's word; z: the
    // vertices per counted item; w: its index in `clears`.
    live: vec4<u32>,
};

// One caster slot's caustic view; field for field with `CausticFit` in
// `scene_shading.wgsl`.
struct CausticFit {
    view_proj: mat4x4<f32>,
    inverse_view_proj: mat4x4<f32>,
    // The caster slot + 1; 0 when there is no clear surface to light.
    slot: u32,
    bias: f32,
    intensity: f32,
    // The view's layer of the caustic map and surface.
    layer: u32,
};

struct MeshVertex {
    position: vec3<f32>, _pad0: f32,
    normal: vec3<f32>, _pad1: f32,
    uv: vec2<f32>, _pad2: vec2<f32>,
    tangent: vec4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> s: Splat;
@group(0) @binding(1) var front: texture_depth_2d;
@group(0) @binding(2) var shadow_maps: texture_depth_2d_array;
@group(0) @binding(3) var<storage, read_write> sums: array<atomic<u32>>;
@group(0) @binding(4) var caustic_out: texture_storage_2d<rgba16float, write>;
// One view per caster slot: `cs_fit` writes this run's at 5; every pass
// after it reads the same buffer at 6 (`ca_fit`).
@group(0) @binding(5) var<storage, read_write> fits_out: array<CausticFit>;
@group(0) @binding(6) var<storage, read> fits: array<CausticFit>;
@group(0) @binding(7) var<storage, read> vertices: array<MeshVertex>;
@group(0) @binding(8) var<uniform> clear: Clear;
@group(0) @binding(9) var front_normal: texture_2d<f32>;
@group(0) @binding(10) var back: texture_depth_2d;
@group(0) @binding(11) var back_normal: texture_2d<f32>;
@group(0) @binding(12) var<storage, read> counts: array<u32>;
// The clear surfaces' reach in the light's view: the largest x, y, -x and -y,
// each kept in order by `ca_order`.
@group(0) @binding(13) var<storage, read_write> bounds: array<atomic<u32>, 4>;
// Every clear surface this frame, by the index its texels carry.
@group(0) @binding(14) var<uniform> clears: array<Clear, CA_MAX_CLEARS>;

// Light per photon in the sums' fixed point.
const CA_FIXED: f32 = 65536.0;
// The most texels either side the surface's slope is taken across.
const CA_MAX_SPAN: i32 = 16;
// Refinements of where the refracted ray meets the scene or the far side.
const CA_STEPS: u32 = 2u;
// The brightest a focus reads, times the light's own, so a sharp focus
// stays a bright line and never a sparkle.
const CA_MAX: f32 = 4.0;
// The narrowed view's margin round the clear surfaces, as a share of their
// size.
const CA_FIT_MARGIN: f32 = 0.05;
// The thinnest a clear surface is between its sides, in metres, for the light
// to leave through its far side: an open sheet, such as a pool's surface, has
// its far side on its near one, and its light ends on the floor.
const CA_MIN_THICKNESS: f32 = 0.002;
const CA_BOUNDS_GROUP: u32 = 256u;
// The most clear surfaces one frame lights (`MAX_CLEARS` in caustics.rs).
const CA_MAX_CLEARS: u32 = 32u;

// A float as a u32 in the same order, so atomicMax over the u32 is the
// largest float; 0 lies below every float.
fn ca_order(x: f32) -> u32 {
    let b = bitcast<u32>(x);
    return select(b | 0x80000000u, ~b, (b & 0x80000000u) != 0u);
}

fn ca_unorder(u: u32) -> f32 {
    return bitcast<f32>(select(~u, u & 0x7fffffffu, (u & 0x80000000u) != 0u));
}

var<workgroup> ca_reach: array<vec4<f32>, 256>;

// This run's caustic view.
fn ca_fit() -> CausticFit {
    return fits[s.sizes.y];
}

// The unit direction toward the light from `p`.
fn ca_toward_light(p: vec3<f32>) -> vec3<f32> {
    return select(s.light.xyz, normalize(s.light.xyz - p), s.light.w > 0.5);
}

// One clear surface's live vertices in the light's view, reduced per group
// and folded into `bounds`. A vertex behind a point or spot light reaches
// the whole view.
@compute @workgroup_size(256)
fn cs_bounds(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_index) li: u32,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    var live = arrayLength(&vertices);
    if clear.live.x == 1u {
        live = min(live, counts[clear.live.y] * clear.live.z);
    }
    var reach = vec4<f32>(-3.0e38);
    for (var i = gid.x; i < live; i = i + groups.x * CA_BOUNDS_GROUP) {
        let clip = s.view_proj * (clear.model * vec4<f32>(vertices[i].position, 1.0));
        if clip.w <= 0.0 {
            reach = vec4<f32>(1.0);
            continue;
        }
        let xy = clip.xy / clip.w;
        reach = max(reach, vec4<f32>(xy, -xy));
    }
    ca_reach[li] = reach;
    workgroupBarrier();
    for (var step = CA_BOUNDS_GROUP / 2u; step > 0u; step = step / 2u) {
        if li < step {
            ca_reach[li] = max(ca_reach[li], ca_reach[li + step]);
        }
        workgroupBarrier();
    }
    if li == 0u && ca_reach[0].x > -1.0e38 {
        let r = ca_reach[0];
        atomicMax(&bounds[0], ca_order(r.x));
        atomicMax(&bounds[1], ca_order(r.y));
        atomicMax(&bounds[2], ca_order(r.z));
        atomicMax(&bounds[3], ca_order(r.w));
    }
}

// The light's view narrowed to a square round the clear surfaces' bounds,
// within its own view. The narrowing remaps clip x and y, so it holds for a
// perspective view as for a sun's.
@compute @workgroup_size(1)
fn cs_fit() {
    let slot = s.sizes.y;
    fits_out[slot].slot = 0u;
    fits_out[slot].bias = s.bias.x;
    fits_out[slot].intensity = s.bias.y;
    fits_out[slot].layer = s.sizes.w;
    if atomicLoad(&bounds[0]) == 0u {
        return;
    }
    let high = vec2<f32>(ca_unorder(atomicLoad(&bounds[0])), ca_unorder(atomicLoad(&bounds[1])));
    let low = -vec2<f32>(ca_unorder(atomicLoad(&bounds[2])), ca_unorder(atomicLoad(&bounds[3])));
    let centre = 0.5 * (low + high);
    let half = 0.5 * max(high.x - low.x, high.y - low.y) * (1.0 + CA_FIT_MARGIN);
    let lo = max(centre - vec2<f32>(half), vec2<f32>(-1.0));
    let hi = min(centre + vec2<f32>(half), vec2<f32>(1.0));
    if any(lo >= hi) {
        return;
    }
    // Clip x and y from [lo, hi] onto [-1, 1]; depth stays the light's.
    let scale = 2.0 / (hi - lo);
    let offset = -(hi + lo) / (hi - lo);
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
    fits_out[slot].view_proj = narrow * s.view_proj;
    fits_out[slot].inverse_view_proj = s.inverse_view_proj * widen;
    fits_out[slot].slot = s.sizes.z;
}

struct CaSurfaceOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

// A clear surface in the caustic map's view, with its world normal.
@vertex
fn vs_surface(@builtin(vertex_index) vid: u32) -> CaSurfaceOut {
    let v = vertices[vid];
    let m = mat3x3<f32>(clear.model[0].xyz, clear.model[1].xyz, clear.model[2].xyz);
    // The cofactor matrix carries normals under any scale; a mirroring model
    // turns them back outward.
    let cofactor = mat3x3<f32>(cross(m[1], m[2]), cross(m[2], m[0]), cross(m[0], m[1]));
    let outward = select(1.0, -1.0, dot(m[0], cofactor[0]) < 0.0);
    var out: CaSurfaceOut;
    out.position = ca_fit().view_proj * (clear.model * vec4<f32>(v.position, 1.0));
    out.normal = cofactor * v.normal * outward;
    return out;
}

// The unit normal, or zero where the mesh has none, and which clear surface
// this is, counted from 1.
@fragment
fn fs_normal(in: CaSurfaceOut) -> @location(0) vec4<f32> {
    let size = length(in.normal);
    return vec4<f32>(select(vec3<f32>(0.0), in.normal / size, size > 0.0), f32(clear.live.w + 1u));
}

fn ca_uv(ndc: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
}

fn ca_ndc(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
}

// The shadow view holding x: a point light's cube face, in the order of
// `caster_face` in `scene_shading.wgsl`; else the light's own.
fn ca_shadow_view(x: vec3<f32>) -> u32 {
    if s.light.w < 1.5 {
        return 0u;
    }
    let d = x - s.light.xyz;
    let a = abs(d);
    if a.x >= a.y && a.x >= a.z {
        return select(1u, 0u, d.x >= 0.0);
    }
    if a.y >= a.z {
        return select(3u, 2u, d.y >= 0.0);
    }
    return select(5u, 4u, d.z >= 0.0);
}

// The opaque point the light sees along its ray through x, from its shadow
// map (which never holds a clear surface); w is 0 off the map or where
// nothing is.
fn ca_receiver(x: vec3<f32>) -> vec4<f32> {
    let view = ca_shadow_view(x);
    let c = s.shadow_views[view] * vec4<f32>(x, 1.0);
    if c.w <= 0.0 {
        return vec4<f32>(0.0);
    }
    let uv = ca_uv(c.xy / c.w);
    if any(uv < vec2<f32>(0.0)) || any(uv >= vec2<f32>(1.0)) {
        return vec4<f32>(0.0);
    }
    // The light renders into the top-left (1 / texel)^2 of its layer.
    let depth = textureLoad(shadow_maps, vec2<i32>(uv / s.bias.z), i32(s.sizes.y + view), 0);
    if depth >= 1.0 {
        return vec4<f32>(0.0);
    }
    let h = s.inverse_shadow_views[view] * vec4<f32>(ca_ndc(uv), depth, 1.0);
    return vec4<f32>(h.xyz / h.w, 1.0);
}

fn ca_on_map(px: vec2<i32>) -> bool {
    return all(px >= vec2<i32>(0)) && all(px < vec2<i32>(i32(s.sizes.x)));
}

fn ca_unproject(px: vec2<i32>, depth: f32) -> vec3<f32> {
    let uv = (vec2<f32>(px) + vec2<f32>(0.5)) / f32(s.sizes.x);
    let h = ca_fit().inverse_view_proj * vec4<f32>(ca_ndc(uv), depth, 1.0);
    return h.xyz / h.w;
}

// The caustic map's texel the light sees x in.
fn ca_texel(x: vec3<f32>) -> vec2<i32> {
    let c = ca_fit().view_proj * vec4<f32>(x, 1.0);
    return vec2<i32>(floor(ca_uv(c.xy / c.w) * f32(s.sizes.x)));
}

// The clear surface's side toward the light at texel `px`; w is 0 off the map
// or where it is not.
fn ca_surface(px: vec2<i32>) -> vec4<f32> {
    if !ca_on_map(px) {
        return vec4<f32>(0.0);
    }
    let depth = textureLoad(front, px, 0);
    if depth >= 1.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(ca_unproject(px, depth), 1.0);
}

// Its far side at texel `px`; w is 0 off the map or where it is not.
fn ca_far(px: vec2<i32>) -> vec4<f32> {
    if !ca_on_map(px) {
        return vec4<f32>(0.0);
    }
    let depth = textureLoad(back, px, 0);
    if depth <= 0.0 {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(ca_unproject(px, depth), 1.0);
}

// The surface's slope along one axis: across both neighbours when both are
// on it, else to the one that is; w is 0 with neither.
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

// The share Fresnel lets through a side at `cos_i`, by Schlick's
// approximation from the index of refraction.
fn ca_passed(cos_i: f32, ior: f32) -> f32 {
    let r = (ior - 1.0) / (ior + 1.0);
    let f0 = r * r;
    return 1.0 - (f0 + (1.0 - f0) * pow(1.0 - clamp(cos_i, 0.0, 1.0), 5.0));
}

// Where the ray from `start` along `d` meets the opaque scene: first the point
// straight under `start`, then the point under where the ray has gone that
// far. w is 0 where it meets nothing.
fn ca_meet_scene(start: vec3<f32>, d: vec3<f32>) -> vec4<f32> {
    var hit = ca_receiver(start);
    if hit.w == 0.0 {
        return hit;
    }
    for (var step = 0u; step < CA_STEPS; step = step + 1u) {
        hit = ca_receiver(start + d * distance(hit.xyz, start));
        if hit.w == 0.0 {
            return hit;
        }
    }
    return hit;
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
    let fit = ca_fit();
    if fit.slot == 0u || any(px >= vec2<i32>(side)) {
        return;
    }
    let here = ca_surface(px);
    if here.w == 0.0 {
        return;
    }
    let marked = textureLoad(front_normal, px, 0);
    let index = u32(marked.a + 0.5);
    if index == 0u || index > CA_MAX_CLEARS {
        return;
    }
    let c = clears[index - 1u];
    let p = here.xyz;
    let sun = ca_toward_light(p);
    var n: vec3<f32>;
    if c.through.w > 0.5 {
        // One texel in metres: 2 / side of clip space over the view's clip x
        // per metre at p's depth (clip w, 1 for a sun). The slope spans about
        // one display cell.
        let row_x = vec3<f32>(fit.view_proj[0].x, fit.view_proj[1].x, fit.view_proj[2].x);
        let w = (fit.view_proj * vec4<f32>(p, 1.0)).w;
        let texel_metres = 2.0 * w / (f32(side) * length(row_x));
        let span = clamp(i32(round(ripple.field.x / texel_metres)), 1, CA_MAX_SPAN);
        let along_x = ca_slope(ca_surface(px + vec2<i32>(span, 0)), ca_surface(px - vec2<i32>(span, 0)), p);
        let along_y = ca_slope(ca_surface(px + vec2<i32>(0, span)), ca_surface(px - vec2<i32>(0, span)), p);
        let across = cross(along_x.xyz, along_y.xyz);
        if along_x.w * along_y.w == 0.0 || !(dot(across, across) > 0.0) {
            return;
        }
        n = normalize(across);
        if dot(n, sun) < 0.0 {
            n = -n;
        }
        // The ripples at one texel's size (`ripple.time.zw` is 1, 0).
        // The foam mask is the camera's; the light's view has no foam.
        if c.through.w < 1.5 {
            n = look_ripple_detail(p, n, texel_metres, 0.0);
        }
    } else {
        n = marked.xyz;
        if !(dot(n, n) > 0.5) {
            return;
        }
        if dot(n, sun) < 0.0 {
            n = -n;
        }
    }
    let cos_in = dot(n, sun);
    if cos_in <= 0.0 {
        return;
    }
    let ior = c.medium.w;
    var light = c.through.rgb * ca_passed(cos_in, ior);
    let d = refract(-sun, n, 1.0 / ior);
    var hit = ca_meet_scene(p, d);
    if hit.w == 0.0 {
        return;
    }
    let t = distance(hit.xyz, p);
    // Where a clear mesh's ray meets its far side, refined as for the scene.
    var exit = vec4<f32>(0.0);
    var inside = 0.0;
    if c.through.w < 0.5 {
        exit = ca_far(px);
    }
    if exit.w > 0.0 {
        inside = distance(exit.xyz, p);
        for (var step = 0u; step < CA_STEPS; step = step + 1u) {
            exit = ca_far(ca_texel(p + d * inside));
            if exit.w == 0.0 {
                break;
            }
            inside = distance(exit.xyz, p);
        }
    }
    if exit.w > 0.0 && inside > CA_MIN_THICKNESS && inside < t {
        // Out through the far side before the scene: the light bends again
        // there and goes on through the air.
        var m = textureLoad(back_normal, ca_texel(exit.xyz), 0).xyz;
        if !(dot(m, m) > 0.5) {
            return;
        }
        if dot(m, d) < 0.0 {
            m = -m;
        }
        let leaving = refract(d, -m, ior);
        // Reflected inside: its light leaves somewhere else.
        if !(dot(leaving, leaving) > 0.0) {
            return;
        }
        light = light * exp(-c.medium.rgb * inside) * ca_passed(dot(leaving, m), ior);
        hit = ca_meet_scene(exit.xyz, leaving);
        if hit.w == 0.0 {
            return;
        }
    } else {
        light = light * exp(-c.medium.rgb * t);
    }
    // Bilinear into the four texels round the hit.
    let landed = fit.view_proj * vec4<f32>(hit.xyz, 1.0);
    let at = ca_uv(landed.xy / landed.w) * f32(side) - vec2<f32>(0.5);
    let low = floor(at);
    let f = at - low;
    let base = vec2<i32>(low);
    ca_add(base, side, light * (1.0 - f.x) * (1.0 - f.y));
    ca_add(base + vec2<i32>(1, 0), side, light * f.x * (1.0 - f.y));
    ca_add(base + vec2<i32>(0, 1), side, light * (1.0 - f.x) * f.y);
    ca_add(base + vec2<i32>(1, 1), side, light * f.x * f.y);
}

// The sums blurred by a 5 x 5 Gaussian of one texel (sigma 1), as light
// relative to the light's own, capped at CA_MAX. A view with no clear
// surface in it writes nothing: shading reads it off by its slot.
@compute @workgroup_size(16, 16)
fn cs_resolve(@builtin(global_invocation_id) gid: vec3<u32>) {
    let side = i32(s.sizes.x);
    let px = vec2<i32>(gid.xy);
    if ca_fit().slot == 0u || any(px >= vec2<i32>(side)) {
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
