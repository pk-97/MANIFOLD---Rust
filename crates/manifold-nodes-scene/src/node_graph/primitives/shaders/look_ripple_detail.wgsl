// Look hook: ripple detail on the water surface. Called from the water
// shading with the surface normal at the hit. The field's level set is smooth
// below about four cells; this adds the small waves as detail normals from a
// tileable slope texture made from the ocean's spectrum (`ripple_detail.rs`).
//
// The slope tile is projected on the three world axes and weighted by the
// normal (triplanar). It is carried along the sim's own velocity by two copies
// half a flow cycle apart that crossfade, so the pattern moves with the water
// and its jump back to the start is never seen (Vlachos 2010, "Water Flow in
// Portal 2"). The strength follows the flow: still water keeps no ripples.
// The mip level is chosen by the size of a pixel on the water, so far water
// averages its ripples out instead of shimmering.
//
// Binds 62-65 itself; the mesh shader includes this file but never calls the
// hook, so meshes bind none of them.

struct RippleUniforms {
    world_to_local: mat4x4<f32>,
    local_to_world: mat4x4<f32>,
    lattice_min: vec4<f32>,  // xyz object-space min corner, w tile side in metres (0: no ripples)
    spacing: vec4<f32>,      // xyz node spacing, w strength override
    nodes: vec4<u32>,        // xyz node counts, w unused
    time: vec4<f32>,         // x seconds, y flow period, z metres per pixel per metre of distance, w metres per pixel flat
};

@group(0) @binding(62) var ripple_slopes: texture_2d<f32>;
@group(0) @binding(63) var ripple_sampler: sampler;
@group(0) @binding(64) var<uniform> ripple: RippleUniforms;
@group(0) @binding(65) var<storage, read> ripple_velocity: array<f32>;

// Kelvin's minimum phase speed of capillary-gravity waves on clean water in
// m/s, the same value as `MIN_RIPPLE_SPEED` in `ripple_detail.rs`: slower
// flow cannot keep ripples.
const MIN_RIPPLE_SPEED: f32 = 0.2313;

// The triplanar blend raised to this power (GPU Gems 3 ch. 1, Geiss 2007).
const TRIPLANAR_SHARPNESS: f32 = 4.0;

fn ripple_lattice_coord(p: vec3<f32>) -> vec3<f32> {
    return clamp(p / ripple.spacing.xyz, vec3<f32>(0.0), vec3<f32>(ripple.nodes.xyz) - vec3<f32>(1.0));
}

fn ripple_node_velocity(i: vec3<i32>) -> vec3<f32> {
    let hi = vec3<i32>(ripple.nodes.xyz) - vec3<i32>(1);
    let c = clamp(i, vec3<i32>(0), hi);
    let n = u32(c.x) + ripple.nodes.x * (u32(c.y) + ripple.nodes.y * u32(c.z));
    return vec3<f32>(ripple_velocity[3u * n], ripple_velocity[3u * n + 1u], ripple_velocity[3u * n + 2u]);
}

fn ripple_velocity_at(p: vec3<f32>) -> vec3<f32> {
    let g = ripple_lattice_coord(p);
    let b = min(vec3<i32>(floor(g)), vec3<i32>(ripple.nodes.xyz) - vec3<i32>(2));
    let f = g - vec3<f32>(b);
    let c000 = ripple_node_velocity(b);
    let c100 = ripple_node_velocity(b + vec3<i32>(1, 0, 0));
    let c010 = ripple_node_velocity(b + vec3<i32>(0, 1, 0));
    let c110 = ripple_node_velocity(b + vec3<i32>(1, 1, 0));
    let c001 = ripple_node_velocity(b + vec3<i32>(0, 0, 1));
    let c101 = ripple_node_velocity(b + vec3<i32>(1, 0, 1));
    let c011 = ripple_node_velocity(b + vec3<i32>(0, 1, 1));
    let c111 = ripple_node_velocity(b + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

// 0 at or below the minimum ripple speed, rising toward 1 as the flow speeds up.
fn ripple_flow_strength(speed: f32) -> f32 {
    if speed <= MIN_RIPPLE_SPEED {
        return 0.0;
    }
    let r = MIN_RIPPLE_SPEED / speed;
    return 1.0 - r * r;
}

// One plane's slope: two copies of the tile carried along the flow, half a
// cycle apart, blended so the contrast stays constant (Heitz & Neyret 2018).
// `q` is the position and `f` the flow in tile units on the plane's two axes.
fn ripple_plane_slope(q: vec2<f32>, f: vec2<f32>, lod: f32) -> vec2<f32> {
    let period = ripple.time.y;
    let cycle = ripple.time.x / period;
    let ph0 = fract(cycle);
    let ph1 = fract(cycle + 0.5);
    let s0 = textureSampleLevel(ripple_slopes, ripple_sampler, q - f * (ph0 * period), lod).xy;
    let s1 = textureSampleLevel(ripple_slopes, ripple_sampler, q - f * (ph1 * period) + vec2<f32>(0.5), lod).xy;
    // Each copy's weight is zero at its own wrap, so the jump is never seen.
    let w0 = 1.0 - abs(2.0 * ph0 - 1.0);
    let w1 = 1.0 - w0;
    return (s0 * w0 + s1 * w1) / sqrt(w0 * w0 + w1 * w1);
}

fn look_ripple_detail(world_pos: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    let tile = ripple.lattice_min.w;
    if tile <= 0.0 {
        return normal;
    }

    let local = (ripple.world_to_local * vec4<f32>(world_pos, 1.0)).xyz - ripple.lattice_min.xyz;
    let v = (ripple.local_to_world * vec4<f32>(ripple_velocity_at(local), 0.0)).xyz;
    let strength = ripple.spacing.w * ripple_flow_strength(length(v));
    if strength <= 0.0 {
        return normal;
    }

    // The layer by its size on screen: the tile texel against a pixel on the water.
    let footprint = ripple.time.z * distance(u.camera_pos.xyz, world_pos) + ripple.time.w;
    let texel = tile / f32(textureDimensions(ripple_slopes).x);
    let lod = max(log2(footprint / texel), 0.0);

    var w = pow(abs(normal), vec3<f32>(TRIPLANAR_SHARPNESS));
    w = w / (w.x + w.y + w.z);

    let ex = vec3<f32>(1.0, 0.0, 0.0);
    let ey = vec3<f32>(0.0, 1.0, 0.0);
    let ez = vec3<f32>(0.0, 0.0, 1.0);

    // x-plane: axes z, y. y-plane: axes x, z. z-plane: axes x, y.
    let sx = ripple_plane_slope(
        vec2<f32>(world_pos.z, world_pos.y) / tile, vec2<f32>(v.z, v.y) / tile, lod);
    let sy = ripple_plane_slope(
        vec2<f32>(world_pos.x, world_pos.z) / tile, vec2<f32>(v.x, v.z) / tile, lod);
    let sz = ripple_plane_slope(
        vec2<f32>(world_pos.x, world_pos.y) / tile, vec2<f32>(v.x, v.y) / tile, lod);

    let tilt = w.x * (sx.x * ez + sx.y * ey)
        + w.y * (sy.x * ex + sy.y * ez)
        + w.z * (sz.x * ex + sz.y * ey);
    return normalize(normal - strength * tilt);
}
