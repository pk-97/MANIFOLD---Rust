// The wet pass: the water's wet marks (`node.wet_map`, composed after the
// map's layout `manifold_node_engine::scene::wet_map::layout()`) drawn over
// the opaque depth into the wet mask `look_wet_surface.wgsl` reads, at a
// fraction of the render's size: wetness is soft, so a coarse mask read
// bilinear loses nothing and costs a fraction of the pixels. One triangle
// per decal in the half in use, lying in the decal's own plane, in one
// instance whose vertex count the map's own draw arguments hold; each pixel
// of the surface the camera sees takes the decal's share of wetness at its
// distance from the decal's centre, and the mask sums the shares (blend add,
// held at 1). The surface shading draws the film where the sum passes a
// threshold, so the marks over a pixel add like metaballs and neighbouring
// marks melt into one patch with one edge. Every share is scaled by a noise
// of the pixel's own position, the same for every decal over it, so the sum
// is scaled as a whole: a patch's inside stays past the threshold and only
// its edge wanders. A pixel off the decal's plane by more than half its
// radius is another surface (the wall a floor mark meets, the floor under a
// wall mark) and stays dry.
//
// A vertex hands on only its position and its decal's first word, and the
// fragment reads the decal itself: on a floor of tens of thousands of
// decals the pass is bound by what its vertices carry, not by its pixels.
//
// Decals, their normals and their radius live in the water's object space;
// the pixel's surface point is taken there too.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // The water object's model matrix.
    model: mat4x4<f32>,
    // Clip space to the water's object space: its inverse model times the
    // camera's inverse view-projection.
    clip_to_local: mat4x4<f32>,
    // x: render pixels per mask texel along each axis; yzw 0.
    mask_scale: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> map: array<u32>;
@group(0) @binding(2) var opaque_depth: texture_depth_2d;

// A pixel within a quarter radius of the decal's plane is on its surface;
// it fades out by half a radius.
const WP_PLANE_NEAR: f32 = 0.25;
const WP_PLANE_FAR: f32 = 0.5;
// A decal dried below this draws nothing.
const WP_MIN_STRENGTH: f32 = 1.0 / 255.0;
// The edge noise scales a pixel's sum by 1 - WP_WANDER to 1 + WP_WANDER.
const WP_WANDER: f32 = 0.4;

struct DecalOut {
    @builtin(position) position: vec4<f32>,
    // The decal's first word in the map.
    @location(0) @interpolate(flat) word: u32,
};

fn wp_centre(word: u32) -> vec3<f32> {
    return vec3<f32>(
        bitcast<f32>(map[word + WD_POSITION]),
        bitcast<f32>(map[word + WD_POSITION + 1u]),
        bitcast<f32>(map[word + WD_POSITION + 2u]),
    );
}

// A lattice point's random value, 0 to 1 (Teschner et al. 2003 to mix the
// axes, then a 32-bit integer finaliser).
fn wp_hash(c: vec3<i32>) -> f32 {
    var h = (bitcast<u32>(c.x) * 73856093u) ^ (bitcast<u32>(c.y) * 19349663u) ^ (bitcast<u32>(c.z) * 83492791u);
    h = (h ^ (h >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    h = h ^ (h >> 16u);
    return f32(h) * (1.0 / 4294967296.0);
}

// Value noise, 0 to 1: the lattice's values blended smoothly between.
fn wp_value_noise(p: vec3<f32>) -> f32 {
    let i = vec3<i32>(floor(p));
    let f = fract(p);
    let s = f * f * (vec3<f32>(3.0) - 2.0 * f);
    let x00 = mix(wp_hash(i), wp_hash(i + vec3<i32>(1, 0, 0)), s.x);
    let x10 = mix(wp_hash(i + vec3<i32>(0, 1, 0)), wp_hash(i + vec3<i32>(1, 1, 0)), s.x);
    let x01 = mix(wp_hash(i + vec3<i32>(0, 0, 1)), wp_hash(i + vec3<i32>(1, 0, 1)), s.x);
    let x11 = mix(wp_hash(i + vec3<i32>(0, 1, 1)), wp_hash(i + vec3<i32>(1, 1, 1)), s.x);
    return mix(mix(x00, x10, s.y), mix(x01, x11, s.y), s.z);
}

// The edge noise at object-space point `p`, 1 on average: two octaves of
// value noise, a decal radius and a third of one across, so an edge wanders
// at the size of the marks and frays finer.
fn wp_wander(p: vec3<f32>, radius: f32) -> f32 {
    let q = p / radius;
    let n = 0.65 * wp_value_noise(q) + 0.35 * wp_value_noise(3.0 * q + vec3<f32>(17.0));
    return 1.0 + WP_WANDER * (2.0 * n - 1.0);
}

fn wp_strength(word: u32) -> f32 {
    return wd_strength(
        map[word + WD_TOUCH], map[word + WD_AMOUNT],
        bitcast<f32>(map[WD_NOW]), bitcast<f32>(map[WD_DRYING]),
    );
}

@vertex
fn vs_decal(@builtin(vertex_index) vid: u32) -> DecalOut {
    var out: DecalOut;
    let word = wd_decal_word(map[WD_ACTIVE], vid / WD_DECAL_VERTICES);
    out.word = word;
    // A triangle at one point covers no pixel.
    out.position = vec4<f32>(0.0);
    if wp_strength(word) < WP_MIN_STRENGTH {
        return out;
    }
    let centre = wp_centre(word);
    let normal = wd_normal(map[word + WD_NORMAL]);
    let radius = bitcast<f32>(map[WD_RADIUS]);
    // Two axes in the decal's plane.
    let away = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.9);
    let side = normalize(cross(normal, away));
    let along = cross(normal, side);
    // The equilateral triangle round the decal's disc: its corners twice the
    // radius out.
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0), vec2<f32>(-1.7320508, -1.0), vec2<f32>(1.7320508, -1.0),
    );
    let c = corners[vid % WD_DECAL_VERTICES] * radius;
    let corner = centre + c.x * side + c.y * along;
    // No depth test: the true clip position lets the rasteriser clip the
    // triangle at the camera.
    out.position = u.view_proj * (u.model * vec4<f32>(corner, 1.0));
    return out;
}

@fragment
fn fs_decal(in: DecalOut) -> @location(0) f32 {
    let size = vec2<i32>(textureDimensions(opaque_depth));
    // The render pixel at this mask texel's centre.
    let pixel = min(vec2<i32>(in.position.xy * u.mask_scale.x), size - vec2<i32>(1));
    // The reversed depth reads 0 where nothing opaque was drawn.
    let depth = textureLoad(opaque_depth, pixel, 0);
    if depth <= 0.0 {
        discard;
    }
    let radius = bitcast<f32>(map[WD_RADIUS]);
    let uv = (vec2<f32>(pixel) + vec2<f32>(0.5)) / vec2<f32>(size);
    let h = u.clip_to_local * vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), depth, 1.0);
    let local = h.xyz / h.w;
    let offset = local - wp_centre(in.word);
    let d = length(offset);
    let height = abs(dot(offset, wd_normal(map[in.word + WD_NORMAL])));
    if d >= radius || height >= WP_PLANE_FAR * radius {
        discard;
    }
    let on_plane = 1.0 - smoothstep(WP_PLANE_NEAR * radius, WP_PLANE_FAR * radius, height);
    return wp_strength(in.word) * wd_falloff(d, radius) * on_plane * wp_wander(local, radius);
}
