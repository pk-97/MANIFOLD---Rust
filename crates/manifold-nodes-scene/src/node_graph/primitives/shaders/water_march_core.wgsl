// The water march's uniforms, field sampling and hit search, shared by the
// compute march (`water_march.wgsl`), the mist (`water_mist.wgsl`) and the
// cover draws (`water_cover.wgsl`). Starts after `world_tiles_read.wgsl` and
// `field_tiles.wgsl`: the level set is a pooled field under the field's
// world-tile table, node p at (p - 1/2) cells in the object's space.

struct MarchUniforms {
    inv_view_proj: mat4x4<f32>,       // clip -> world, this frame's (jittered) raster matrix
    view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
    world_to_local: mat4x4<f32>,      // inverse of the object's model matrix
    local_to_world: mat4x4<f32>,      // the model matrix
    prev_local_to_world: mat4x4<f32>, // last frame's model matrix
    cell: vec4<f32>,                  // x display cell size in metres, y frame delta in seconds, zw unused
    tiles: vec4<u32>,                 // x pool tiles, y table entries, z slot + 1, w unused
    target_size: vec4<f32>,           // x width, y height, z = 1 if a velocity array is bound, w unused
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
const MARCH_REFINES: u32 = 8u;          // most false-position steps on the sign change
const MARCH_HIT_CELLS: f32 = 0.001;     // a refine this near zero is the hit
const EXIT_MIN_STEP_CELLS: f32 = 0.1;   // prototype `exit` (main.rs:479): smallest step inside the water
// A step over an unheld tile lands this far past its face, so the next sample
// is in the next tile whatever the rounding.
const TILE_EXIT_NUDGE_CELLS: f32 = 0.01;
// Safety cap so a broken field cannot hang the GPU (the prototype's rays
// averaged under 40 samples).
const MARCH_MAX_STEPS: u32 = 1024u;

fn level_at(c: FieldCorners) -> f32 {
    return field_mix(
        levelset[c.c000], levelset[c.c100], levelset[c.c010], levelset[c.c110],
        levelset[c.c001], levelset[c.c101], levelset[c.c011], levelset[c.c111],
        c.f,
    );
}

fn node_value(n: vec3<i32>) -> f32 {
    return levelset[ts_cell_at(n)];
}

// The level set at an object-space point: the trilinear of the node values at
// q = p / cell + 1/2.
fn sample_field(p: vec3<f32>) -> f32 {
    let q = field_node_coord(p, u.cell.x);
    field_enter(q);
    return level_at(field_corners(q));
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

// The world ray in the object's space: origin, unit direction, and the
// world-to-local scale `s` of the direction.
fn local_ray(origin_w: vec3<f32>, dir_w: vec3<f32>) -> LocalRay {
    let o = (u.world_to_local * vec4<f32>(origin_w, 1.0)).xyz;
    let d_raw = (u.world_to_local * vec4<f32>(dir_w, 0.0)).xyz;
    let s = length(d_raw);
    let d = d_raw / s;
    return LocalRay(o, d, s);
}

// Slab test against the field's extent, widened by a cell on every side:
// (t0, t1) along the local ray, t0 >= 0. An empty extent gives t0 > t1, no hit.
fn lattice_span(o: vec3<f32>, d: vec3<f32>) -> vec2<f32> {
    let lo = field_extent_min();
    let hi = field_extent_max();
    if any(lo > hi) {
        return vec2<f32>(1.0, 0.0);
    }
    let pad = vec3<f32>(u.cell.x);
    let inv_d = 1.0 / d;
    let ta = (lo - pad - o) * inv_d;
    let tb = (hi + pad - o) * inv_d;
    let tmin = min(ta, tb);
    let tmax = max(ta, tb);
    var t0 = max(max(tmin.x, tmin.y), tmin.z);
    let t1 = min(min(tmax.x, tmax.y), tmax.z);
    t0 = max(t0, 0.0);
    return vec2<f32>(t0, t1);
}

// The march's view of a sample: the field there and, when the table does not
// hold the sample's tile, the distance along the ray to where that tile ends.
struct FieldAhead {
    phi: f32,
    skip: f32,
};

// The distance along the unit ray `d` from node coordinate `q` to the far face
// of the 8-node tile holding node floor(q), and a nudge past it.
fn tile_exit(q: vec3<f32>, d: vec3<f32>) -> f32 {
    let cell = u.cell.x;
    let lo = floor(q / 8.0) * 8.0;
    let to_face = select(q - lo, lo + vec3<f32>(8.0) - q, d > vec3<f32>(0.0));
    let along = to_face * cell / max(abs(d), vec3<f32>(1e-12));
    return min(along.x, min(along.y, along.z)) + TILE_EXIT_NUDGE_CELLS * cell;
}

// An unheld tile is the canonical far value everywhere, so the ray steps to its
// exit at once instead of in field-value steps.
fn field_ahead(p: vec3<f32>, d: vec3<f32>) -> FieldAhead {
    let q = field_node_coord(p, u.cell.x);
    if !field_enter(q) {
        return FieldAhead(FIELD_FAR_CELLS * u.cell.x, tile_exit(q, d));
    }
    return FieldAhead(level_at(field_corners(q)), 0.0);
}

// The first surface crossing along the local ray within [t0, t1], or -1.0
// when the ray leaves the span without one.
fn march_surface(o: vec3<f32>, d: vec3<f32>, t0: f32, t1: f32) -> f32 {
    let cell = u.cell.x;
    let near = MARCH_NEAR_CELLS * cell;
    let min_step = MARCH_MIN_STEP_CELLS * cell;
    var t = t0 + 1e-4 * cell;
    var here = field_ahead(o + d * t, d);
    if here.phi <= 0.0 { return t; }
    for (var i = 0u; i < MARCH_MAX_STEPS; i++) {
        var advance = here.skip;
        if here.skip == 0.0 {
            advance = select(here.phi, max(MARCH_NEAR_STEP * here.phi, min_step), here.phi < near);
        }
        let prev_t = t;
        t += advance;
        if t > t1 { return -1.0; }
        let next = field_ahead(o + d * t, d);
        if next.phi <= 0.0 {
            // False position on the bracket: the field is near linear across
            // the last step, so a line root usually lands on the crossing at
            // once. The end that stays has its value halved (Illinois), so a
            // curved stretch or a skipped tile's stand-in value cannot stall
            // the bracket.
            let hit = MARCH_HIT_CELLS * cell;
            var a = prev_t;
            var fa = here.phi;
            var b = t;
            var fb = next.phi;
            for (var k = 0u; k < MARCH_REFINES; k++) {
                let m = b - fb * (b - a) / min(fb - fa, -1e-20);
                let fm = sample_field(o + d * m);
                if abs(fm) < hit { return m; }
                if fm > 0.0 {
                    a = m;
                    fa = fm;
                    fb = 0.5 * fb;
                } else {
                    b = m;
                    fb = fm;
                    fa = 0.5 * fa;
                }
            }
            return b - fb * (b - a) / min(fb - fa, -1e-20);
        }
        here = next;
    }
    return -1.0;
}
