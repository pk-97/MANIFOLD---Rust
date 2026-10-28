// The wet pass: the water's wet marks (`node.wet_map`, composed after the
// map's layout `manifold_node_engine::scene::wet_map::layout()`) drawn over
// the opaque depth into the wet mask `look_wet_surface.wgsl` reads, at a
// fraction of the render's size: wetness is soft, so a coarse mask read
// bilinear loses nothing and costs a fraction of the pixels. One quad
// per decal in the half in use, lying in the decal's own plane, the count
// from the map's own draw arguments; each pixel of the surface the camera
// sees takes the decal's wetness at its distance from the decal's centre,
// and the mask keeps the wettest (blend max). A pixel off the decal's plane
// by more than half its radius is another surface (the wall a floor mark
// meets, the floor under a wall mark) and stays dry.
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

struct DecalOut {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) centre: vec3<f32>,
    @location(1) @interpolate(flat) normal: vec3<f32>,
    @location(2) @interpolate(flat) strength: f32,
    @location(3) @interpolate(flat) radius: f32,
};

@vertex
fn vs_decal(@builtin(vertex_index) vid: u32, @builtin(instance_index) iid: u32) -> DecalOut {
    var out: DecalOut;
    // A quad at one point covers no pixel.
    out.position = vec4<f32>(0.0);
    out.centre = vec3<f32>(0.0);
    out.normal = vec3<f32>(0.0);
    out.strength = 0.0;
    out.radius = 0.0;
    let word = wd_decal_word(map[WD_ACTIVE], iid);
    let strength = wd_strength(
        map[word + WD_TOUCH], map[word + WD_AMOUNT],
        bitcast<f32>(map[WD_NOW]), bitcast<f32>(map[WD_DRYING]),
    );
    if strength < WP_MIN_STRENGTH {
        return out;
    }
    let centre = vec3<f32>(
        bitcast<f32>(map[word + WD_POSITION]),
        bitcast<f32>(map[word + WD_POSITION + 1u]),
        bitcast<f32>(map[word + WD_POSITION + 2u]),
    );
    let normal = wd_normal(map[word + WD_NORMAL]);
    let radius = bitcast<f32>(map[WD_RADIUS]);
    // Two axes in the decal's plane.
    let away = select(vec3<f32>(0.0, 1.0, 0.0), vec3<f32>(1.0, 0.0, 0.0), abs(normal.y) > 0.9);
    let side = normalize(cross(normal, away));
    let along = cross(normal, side);
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let c = corners[vid] * radius;
    let corner = centre + c.x * side + c.y * along;
    // No depth test: the true clip position lets the rasteriser clip the
    // quad at the camera.
    out.position = u.view_proj * (u.model * vec4<f32>(corner, 1.0));
    out.centre = centre;
    out.normal = normal;
    out.strength = strength;
    out.radius = radius;
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
    let uv = (vec2<f32>(pixel) + vec2<f32>(0.5)) / vec2<f32>(size);
    let h = u.clip_to_local * vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), depth, 1.0);
    let offset = h.xyz / h.w - in.centre;
    let d = length(offset);
    let height = abs(dot(offset, in.normal));
    if d >= in.radius || height >= WP_PLANE_FAR * in.radius {
        discard;
    }
    let on_plane = 1.0 - smoothstep(WP_PLANE_NEAR * in.radius, WP_PLANE_FAR * in.radius, height);
    return in.strength * wd_falloff(d, in.radius) * on_plane;
}
