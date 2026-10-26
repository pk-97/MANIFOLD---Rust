// The water march's uniforms, field sampling and hit search, shared by the
// compute march (`water_march.wgsl`) and the cover draws (`water_cover.wgsl`).

struct MarchUniforms {
    inv_view_proj: mat4x4<f32>,       // clip -> world, this frame's (jittered) raster matrix
    view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    world_to_local: mat4x4<f32>,      // inverse of the object's model matrix
    local_to_world: mat4x4<f32>,      // the model matrix
    prev_local_to_world: mat4x4<f32>, // last frame's model matrix
    lattice_min: vec4<f32>,           // xyz object-space min corner, w = cell (smallest spacing)
    spacing: vec4<f32>,               // xyz node spacing, w = frame delta in seconds
    nodes: vec4<u32>,                 // xyz node counts, w = slot + 1
    target_size: vec4<f32>,           // x width, y height, z = 1 if a velocity lattice is bound, w unused
    jitter: vec4<f32>,                // xy this frame's jitter in NDC, zw last frame's
    flags: vec4<u32>,                 // x: bit 0 write depth_out, bit 1 write velocity_out
};

@group(0) @binding(0) var<uniform> u: MarchUniforms;
@group(0) @binding(1) var<storage, read> levelset: array<f32>;

// Constants below: prototype `levelset_proto/src/round3.rs` `march3` (the Safe
// march, measured on the Dam Break at 64 and 128; GPU_FLIP_END_GAME_DESIGN.md D2 step 6).
const MARCH_NEAR_CELLS: f32 = 2.0;      // within this distance of the surface the step shrinks
const MARCH_NEAR_STEP: f32 = 0.6;       // near the surface, step = 0.6 x field value
const MARCH_MIN_STEP_CELLS: f32 = 0.05; // smallest step, so a grazing ray still advances
const MARCH_BISECTIONS: u32 = 8u;       // bisections on the sign change
const EXIT_MIN_STEP_CELLS: f32 = 0.1;   // prototype `exit` (main.rs:479): smallest step inside the water
// Safety cap so a broken field cannot hang the GPU (the prototype's rays
// averaged under 40 samples).
const MARCH_MAX_STEPS: u32 = 1024u;

fn node_value(i: vec3<i32>) -> f32 {
    let hi = vec3<i32>(u.nodes.xyz) - vec3<i32>(1);
    let c = clamp(i, vec3<i32>(0), hi);
    let n = u32(c.x) + u.nodes.x * (u32(c.y) + u.nodes.y * u32(c.z));
    return levelset[n];
}

// Lattice coordinates (node units) of `p`, relative to the min corner, in metres.
fn lattice_coord(p: vec3<f32>) -> vec3<f32> {
    return clamp(p / u.spacing.xyz, vec3<f32>(0.0), vec3<f32>(u.nodes.xyz) - vec3<f32>(1.0));
}

fn sample_field(p: vec3<f32>) -> f32 {
    let g = lattice_coord(p);
    let b = min(vec3<i32>(floor(g)), vec3<i32>(u.nodes.xyz) - vec3<i32>(2));
    let f = g - vec3<f32>(b);
    let c000 = node_value(b);
    let c100 = node_value(b + vec3<i32>(1, 0, 0));
    let c010 = node_value(b + vec3<i32>(0, 1, 0));
    let c110 = node_value(b + vec3<i32>(1, 1, 0));
    let c001 = node_value(b + vec3<i32>(0, 0, 1));
    let c101 = node_value(b + vec3<i32>(1, 0, 1));
    let c011 = node_value(b + vec3<i32>(0, 1, 1));
    let c111 = node_value(b + vec3<i32>(1, 1, 1));
    let x00 = mix(c000, c100, f.x);
    let x10 = mix(c010, c110, f.x);
    let x01 = mix(c001, c101, f.x);
    let x11 = mix(c011, c111, f.x);
    return mix(mix(x00, x10, f.y), mix(x01, x11, f.y), f.z);
}

fn unproject(ndc: vec2<f32>, raw_depth: f32) -> vec3<f32> {
    let w = u.inv_view_proj * vec4<f32>(ndc, raw_depth, 1.0);
    return w.xyz / w.w;
}

struct LocalRay {
    o: vec3<f32>,
    d: vec3<f32>,
    s: f32,
};

// The world ray in the lattice's frame: origin relative to the min corner,
// unit direction, and the world-to-local scale `s` of the direction.
fn local_ray(origin_w: vec3<f32>, dir_w: vec3<f32>) -> LocalRay {
    let o = (u.world_to_local * vec4<f32>(origin_w, 1.0)).xyz - u.lattice_min.xyz;
    let d_raw = (u.world_to_local * vec4<f32>(dir_w, 0.0)).xyz;
    let s = length(d_raw);
    let d = d_raw / s;
    return LocalRay(o, d, s);
}

// Slab test against the lattice box: (t0, t1) along the local ray, t0 >= 0.
fn lattice_span(o: vec3<f32>, d: vec3<f32>) -> vec2<f32> {
    let box_max = u.spacing.xyz * (vec3<f32>(u.nodes.xyz) - vec3<f32>(1.0));
    let inv_d = 1.0 / d;
    let ta = (vec3<f32>(0.0) - o) * inv_d;
    let tb = (box_max - o) * inv_d;
    let tmin = min(ta, tb);
    let tmax = max(ta, tb);
    var t0 = max(max(tmin.x, tmin.y), tmin.z);
    let t1 = min(min(tmax.x, tmax.y), tmax.z);
    t0 = max(t0, 0.0);
    return vec2<f32>(t0, t1);
}

// The first surface crossing along the local ray within [t0, t1], or -1.0
// when the ray leaves the span without one.
fn march_surface(o: vec3<f32>, d: vec3<f32>, t0: f32, t1: f32) -> f32 {
    let cell = u.lattice_min.w;
    let near = MARCH_NEAR_CELLS * cell;
    let min_step = MARCH_MIN_STEP_CELLS * cell;
    var t = t0 + 1e-4 * cell;
    var prev_t = t;
    var prev_phi = sample_field(o + d * t);
    var t_hit = t;
    var found = prev_phi <= 0.0;
    if !found {
        for (var i = 0u; i < MARCH_MAX_STEPS; i++) {
            let advance = select(prev_phi, max(MARCH_NEAR_STEP * prev_phi, min_step), prev_phi < near);
            t += advance;
            if t > t1 { return -1.0; }
            let phi = sample_field(o + d * t);
            if phi <= 0.0 {
                var a = prev_t;
                var b = t;
                for (var k = 0u; k < MARCH_BISECTIONS; k++) {
                    let m = 0.5 * (a + b);
                    if sample_field(o + d * m) > 0.0 { a = m; } else { b = m; }
                }
                t_hit = 0.5 * (a + b);
                found = true;
                break;
            }
            prev_t = t;
            prev_phi = phi;
        }
    }
    if !found { return -1.0; }
    return t_hit;
}
