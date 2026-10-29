// Look hook: a Terrain's layered ground. Called from `fs_pbr` after the
// object's own material resolves, on a Terrain only (`terrain.on_tiles.x`).
// The object's own material is the ground. The cliff layer covers ground
// steeper than `cliff.x`, whole past `cliff.y`. The peak layer covers ground
// above the snow line, fading over its softness, and slides off ground
// steeper than `snow.z`. Each layer is box mapped in world space at its own
// tile size, so the layers line up across the land and with anything else
// set in the world. Where two layers meet, each one's height (its colour's
// brightness) decides which shows, so the boundary follows the stones
// instead of a smooth fade (height blending). A broad, slow colour drift in
// world space runs across every layer so repeats don't read at distance, and
// the ground layer browns where the ground is dry.
//
// Binds 69..75 itself; only `fs_pbr` calls it.

struct TerrainLook {
    on_tiles: vec4<f32>,
    cliff: vec4<f32>,
    snow: vec4<f32>,
    cliff_maps: vec4<f32>,
    peak_maps: vec4<f32>,
}

@group(0) @binding(69) var<uniform> terrain: TerrainLook;
@group(0) @binding(70) var terrain_cliff_color: texture_2d<f32>;
@group(0) @binding(71) var terrain_cliff_normal: texture_2d<f32>;
@group(0) @binding(72) var terrain_cliff_mr: texture_2d<f32>;
@group(0) @binding(73) var terrain_peak_color: texture_2d<f32>;
@group(0) @binding(74) var terrain_peak_normal: texture_2d<f32>;
@group(0) @binding(75) var terrain_peak_mr: texture_2d<f32>;

// What dry ground does to the ground layer's colour, and how much of it at
// the driest: straw and earth, from a green.
const TERRAIN_DRY_TINT: vec3<f32> = vec3<f32>(1.45, 1.15, 0.6);
const TERRAIN_DRY_SHARE: f32 = 0.7;

// How far a layer's height can lift it over the one under it where they
// meet: the width, in brightness, of the band both show in.
const TERRAIN_BLEND_DEPTH: f32 = 0.2;

struct TerrainSurface {
    albedo: vec3<f32>,
    normal: vec3<f32>,
    roughness: f32,
}

struct TerrainLayer {
    albedo: vec3<f32>,
    normal: vec3<f32>,
    roughness: f32,
    height: f32,
}

// Box-mapping weights from the surface normal, sharpened so each face takes
// one projection except near the seams between them. A projection under
// TERRAIN_FACE_MIN is dropped and the rest renormalized, so most pixels read
// one or two projections, not three.
const TERRAIN_FACE_MIN: f32 = 0.02;

fn terrain_box_weights(n: vec3<f32>) -> vec3<f32> {
    var w = pow(abs(n), vec3<f32>(4.0));
    w /= w.x + w.y + w.z;
    w = select(w, vec3<f32>(0.0), w < vec3<f32>(TERRAIN_FACE_MIN));
    return w / (w.x + w.y + w.z);
}

fn terrain_sign(v: f32) -> f32 {
    return select(-1.0, 1.0, v >= 0.0);
}

// The world point's screen-space derivatives, taken once where control flow
// is uniform so the layers can skip their samples per pixel.
struct TerrainFootprint {
    p: vec3<f32>,
    dx: vec3<f32>,
    dy: vec3<f32>,
}

fn terrain_read(t: texture_2d<f32>, s: sampler, uv: vec2<f32>, dx: vec2<f32>, dy: vec2<f32>) -> vec4<f32> {
    return textureSampleGrad(t, s, uv, dx, dy);
}

// One layer's colour, normal, roughness and height at `at` with surface
// normal `n`, box mapped at `tile` metres.
fn terrain_layer(
    color: texture_2d<f32>,
    normal_map: texture_2d<f32>,
    mr: texture_2d<f32>,
    maps: vec4<f32>,
    at: TerrainFootprint,
    n: vec3<f32>,
    tile: f32,
) -> TerrainLayer {
    let w = terrain_box_weights(n);
    let s = vec3<f32>(terrain_sign(n.x), terrain_sign(n.y), terrain_sign(n.z));
    // Each projection mirrors with the side it faces, so the texture keeps
    // its handedness on every face (Golus 2017).
    let px = vec2<f32>(s.x, 1.0);
    let py = vec2<f32>(s.y, 1.0);
    let pz = vec2<f32>(-s.z, 1.0);
    let uv_x = at.p.zy * px / tile;
    let uv_y = at.p.xz * py / tile;
    let uv_z = at.p.xy * pz / tile;
    let dx_x = at.dx.zy * px / tile;
    let dy_x = at.dy.zy * px / tile;
    let dx_y = at.dx.xz * py / tile;
    let dy_y = at.dy.xz * py / tile;
    let dx_z = at.dx.xy * pz / tile;
    let dy_z = at.dy.xy * pz / tile;
    var albedo = vec3<f32>(0.0);
    var tx = vec3<f32>(0.0, 0.0, 1.0);
    var ty = vec3<f32>(0.0, 0.0, 1.0);
    var tz = vec3<f32>(0.0, 0.0, 1.0);
    var roughness = 0.0;
    if w.x > 0.0 {
        albedo += terrain_read(color, base_color_sampler, uv_x, dx_x, dy_x).rgb * w.x;
        if maps.y > 0.5 {
            tx = terrain_read(normal_map, normal_sampler, uv_x, dx_x, dy_x).xyz * 2.0 - 1.0;
        }
        if maps.z > 0.5 {
            roughness += terrain_read(mr, mr_sampler, uv_x, dx_x, dy_x).g * w.x;
        }
    }
    if w.y > 0.0 {
        albedo += terrain_read(color, base_color_sampler, uv_y, dx_y, dy_y).rgb * w.y;
        if maps.y > 0.5 {
            ty = terrain_read(normal_map, normal_sampler, uv_y, dx_y, dy_y).xyz * 2.0 - 1.0;
        }
        if maps.z > 0.5 {
            roughness += terrain_read(mr, mr_sampler, uv_y, dx_y, dy_y).g * w.y;
        }
    }
    if w.z > 0.0 {
        albedo += terrain_read(color, base_color_sampler, uv_z, dx_z, dy_z).rgb * w.z;
        if maps.y > 0.5 {
            tz = terrain_read(normal_map, normal_sampler, uv_z, dx_z, dy_z).xyz * 2.0 - 1.0;
        }
        if maps.z > 0.5 {
            roughness += terrain_read(mr, mr_sampler, uv_z, dx_z, dy_z).g * w.z;
        }
    }
    if maps.x < 0.5 {
        albedo = vec3<f32>(0.5);
    }
    if maps.z < 0.5 {
        roughness = 0.85;
    }
    var normal = n;
    if maps.y > 0.5 {
        // Whiteout blend (Golus 2017): each projection's tangent-space normal
        // added to the surface normal's own components in its plane.
        tx.x *= s.x;
        ty.x *= s.y;
        tz.x *= -s.z;
        let nx = vec3<f32>(tx.xy + n.zy, abs(tx.z) * n.x);
        let ny = vec3<f32>(ty.xy + n.xz, abs(ty.z) * n.y);
        let nz = vec3<f32>(tz.xy + n.xy, abs(tz.z) * n.z);
        normal = normalize(nx.zyx * w.x + ny.xzy * w.y + nz.xyz * w.z);
    }
    return TerrainLayer(albedo, normal, roughness, dot(albedo, vec3<f32>(0.3333)));
}

// The share of the upper layer where it covers `t` of the ground, with the
// layers' heights `under` and `over`: the taller one wins inside a band
// TERRAIN_BLEND_DEPTH wide.
fn terrain_height_blend(under: f32, over: f32, t: f32) -> f32 {
    let a = under + (1.0 - t);
    let b = over + t;
    let top = max(a, b) - TERRAIN_BLEND_DEPTH;
    let wa = max(a - top, 0.0);
    let wb = max(b - top, 0.0);
    return wb / max(wa + wb, 1e-5);
}

fn terrain_drift_hash(c: vec2<i32>) -> f32 {
    var h = (bitcast<u32>(c.x) * 0x8da6b343u) ^ (bitcast<u32>(c.y) * 0xd8163841u);
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    return f32(h ^ (h >> 16u)) * (1.0 / 4294967296.0);
}

fn terrain_drift_noise(p: vec2<f32>) -> f32 {
    let c = vec2<i32>(floor(p));
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = terrain_drift_hash(c);
    let b = terrain_drift_hash(c + vec2<i32>(1, 0));
    let d = terrain_drift_hash(c + vec2<i32>(0, 1));
    let e = terrain_drift_hash(c + vec2<i32>(1, 1));
    return mix(mix(a, b, u.x), mix(d, e, u.x), u.y) * 2.0 - 1.0;
}

// About [-1, 1]: patches about 60 m and 250 m across.
fn terrain_drift(xz: vec2<f32>) -> f32 {
    return 0.6 * terrain_drift_noise(xz / 250.0) + 0.4 * terrain_drift_noise(xz / 60.0);
}

// The ground at world point `p` with geometric normal `geo` under the
// object's own `albedo`, shading normal `n` and `roughness`. A layer is read
// only where it shows.
fn terrain_ground(p: vec3<f32>, geo: vec3<f32>, albedo: vec3<f32>, n: vec3<f32>, roughness: f32) -> TerrainSurface {
    let at = TerrainFootprint(p, dpdx(p), dpdy(p));
    let drift = terrain_drift(p.xz);
    let steep = 1.0 - geo.y;
    // Ground cover dries and browns on steeper ground and in broad patches,
    // so a hillside of one green material reads as grass, scrub and earth.
    let dry_noise = 0.6 * terrain_drift_noise(p.xz / 140.0 + 17.3) + 0.4 * terrain_drift_noise(p.xz / 35.0 - 5.1);
    let dry = smoothstep(-0.2, 0.5, dry_noise + 4.0 * steep - 0.3);
    var a = mix(albedo, albedo * TERRAIN_DRY_TINT, dry * TERRAIN_DRY_SHARE);
    var nn = n;
    var r = roughness;
    var h = dot(albedo, vec3<f32>(0.3333));
    let cliff_cover = smoothstep(terrain.cliff.x, terrain.cliff.y, steep + 0.06 * drift);
    if dot(terrain.cliff_maps.xyz, vec3<f32>(1.0)) > 0.5 && cliff_cover > 0.0 {
        let cliff = terrain_layer(terrain_cliff_color, terrain_cliff_normal, terrain_cliff_mr, terrain.cliff_maps, at, geo, terrain.on_tiles.y);
        let w = terrain_height_blend(h, cliff.height, cliff_cover);
        a = mix(a, cliff.albedo, w);
        nn = normalize(mix(nn, cliff.normal, w));
        r = mix(r, cliff.roughness, w);
        h = mix(h, cliff.height, w);
    }
    let soft = terrain.snow.y;
    let above = smoothstep(terrain.snow.x - soft, terrain.snow.x + soft, p.y + soft * drift);
    let holds = 1.0 - smoothstep(terrain.snow.z - 0.1, terrain.snow.z + 0.1, steep);
    let snow_cover = above * holds;
    if dot(terrain.peak_maps.xyz, vec3<f32>(1.0)) > 0.5 && snow_cover > 0.0 {
        let peak = terrain_layer(terrain_peak_color, terrain_peak_normal, terrain_peak_mr, terrain.peak_maps, at, geo, terrain.on_tiles.z);
        let w = terrain_height_blend(h, peak.height, snow_cover);
        a = mix(a, peak.albedo, w);
        nn = normalize(mix(nn, peak.normal, w));
        r = mix(r, peak.roughness, w);
    }
    return TerrainSurface(a * (1.0 + 0.12 * drift), nn, r);
}
