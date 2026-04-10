// Nested Cubes — instanced gap-face cubes with EMA-smoothed Y-axis rotation.
//
// Geometry: 6 unwelded quads scaled 0.5 from their face centers (Primitive SOP).
// 5 instances with ramp scaling (1.0 → 2.0) and lagged rotation.
// Two-pass rendering:
//   Pass 1 (vs_main): 36 triangle vertices — solid black occluders
//   Pass 2 (vs_edges): 48 line vertices — white quad outlines (no diagonals)
//
// Scatter parameter displaces each face to a random floating position.
// At scatter=0 faces are in structured cube form, at scatter=1 fully scattered.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // x: size[0], y: size[1], z: size[2], w: size[3]
    sizes_0_3: vec4<f32>,
    // x: angle[0], y: angle[1], z: angle[2], w: angle[3]
    angles_0_3: vec4<f32>,
    // x: size[4], y: angle[4], z: color (0=black, 1=white), w: scatter (0..1)
    extra: vec4<f32>,
    // x: time (seconds), y: snap_envelope (0..1)
    extra2: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// ─── Deterministic hash for per-face random offsets ───────────────

fn hash_u(n: u32) -> u32 {
    var x = n;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return x;
}

fn hash_f(n: u32) -> f32 {
    return f32(hash_u(n) & 0xFFFFu) / 32767.5 - 1.0;
}

// ─── Gap-face cube: triangle geometry (36 verts) ──────────────────

// Front face (+Z)
const FRONT: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(-0.25, -0.25,  0.5),
    vec3<f32>( 0.25, -0.25,  0.5),
    vec3<f32>( 0.25,  0.25,  0.5),
    vec3<f32>(-0.25, -0.25,  0.5),
    vec3<f32>( 0.25,  0.25,  0.5),
    vec3<f32>(-0.25,  0.25,  0.5),
);
// Back face (-Z)
const BACK: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>( 0.25, -0.25, -0.5),
    vec3<f32>(-0.25, -0.25, -0.5),
    vec3<f32>(-0.25,  0.25, -0.5),
    vec3<f32>( 0.25, -0.25, -0.5),
    vec3<f32>(-0.25,  0.25, -0.5),
    vec3<f32>( 0.25,  0.25, -0.5),
);
// Right face (+X)
const RIGHT: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>( 0.5, -0.25,  0.25),
    vec3<f32>( 0.5, -0.25, -0.25),
    vec3<f32>( 0.5,  0.25, -0.25),
    vec3<f32>( 0.5, -0.25,  0.25),
    vec3<f32>( 0.5,  0.25, -0.25),
    vec3<f32>( 0.5,  0.25,  0.25),
);
// Left face (-X)
const LEFT: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(-0.5, -0.25, -0.25),
    vec3<f32>(-0.5, -0.25,  0.25),
    vec3<f32>(-0.5,  0.25,  0.25),
    vec3<f32>(-0.5, -0.25, -0.25),
    vec3<f32>(-0.5,  0.25,  0.25),
    vec3<f32>(-0.5,  0.25, -0.25),
);
// Top face (+Y)
const TOP: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(-0.25,  0.5,  0.25),
    vec3<f32>( 0.25,  0.5,  0.25),
    vec3<f32>( 0.25,  0.5, -0.25),
    vec3<f32>(-0.25,  0.5,  0.25),
    vec3<f32>( 0.25,  0.5, -0.25),
    vec3<f32>(-0.25,  0.5, -0.25),
);
// Bottom face (-Y)
const BOTTOM: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>(-0.25, -0.5, -0.25),
    vec3<f32>( 0.25, -0.5, -0.25),
    vec3<f32>( 0.25, -0.5,  0.25),
    vec3<f32>(-0.25, -0.5, -0.25),
    vec3<f32>( 0.25, -0.5,  0.25),
    vec3<f32>(-0.25, -0.5,  0.25),
);

fn get_tri_vertex(vid: u32) -> vec3<f32> {
    let face = vid / 6u;
    let idx = vid % 6u;
    switch face {
        case 0u: { return FRONT[idx]; }
        case 1u: { return BACK[idx]; }
        case 2u: { return RIGHT[idx]; }
        case 3u: { return LEFT[idx]; }
        case 4u: { return TOP[idx]; }
        default: { return BOTTOM[idx]; }
    }
}

// ─── Gap-face cube: edge geometry (48 verts as line pairs) ────────

const FACE_CORNERS: array<array<vec3<f32>, 4>, 6> = array<array<vec3<f32>, 4>, 6>(
    array<vec3<f32>, 4>(
        vec3<f32>(-0.25, -0.25,  0.5), vec3<f32>( 0.25, -0.25,  0.5),
        vec3<f32>( 0.25,  0.25,  0.5), vec3<f32>(-0.25,  0.25,  0.5),
    ),
    array<vec3<f32>, 4>(
        vec3<f32>( 0.25, -0.25, -0.5), vec3<f32>(-0.25, -0.25, -0.5),
        vec3<f32>(-0.25,  0.25, -0.5), vec3<f32>( 0.25,  0.25, -0.5),
    ),
    array<vec3<f32>, 4>(
        vec3<f32>( 0.5, -0.25,  0.25), vec3<f32>( 0.5, -0.25, -0.25),
        vec3<f32>( 0.5,  0.25, -0.25), vec3<f32>( 0.5,  0.25,  0.25),
    ),
    array<vec3<f32>, 4>(
        vec3<f32>(-0.5, -0.25, -0.25), vec3<f32>(-0.5, -0.25,  0.25),
        vec3<f32>(-0.5,  0.25,  0.25), vec3<f32>(-0.5,  0.25, -0.25),
    ),
    array<vec3<f32>, 4>(
        vec3<f32>(-0.25,  0.5,  0.25), vec3<f32>( 0.25,  0.5,  0.25),
        vec3<f32>( 0.25,  0.5, -0.25), vec3<f32>(-0.25,  0.5, -0.25),
    ),
    array<vec3<f32>, 4>(
        vec3<f32>(-0.25, -0.5, -0.25), vec3<f32>( 0.25, -0.5, -0.25),
        vec3<f32>( 0.25, -0.5,  0.25), vec3<f32>(-0.25, -0.5,  0.25),
    ),
);

const EDGE_INDICES: array<u32, 8> = array<u32, 8>(0u, 1u, 1u, 2u, 2u, 3u, 3u, 0u);

fn get_edge_vertex(vid: u32) -> vec3<f32> {
    let face = vid / 8u;
    let edge_vert = vid % 8u;
    return FACE_CORNERS[face][EDGE_INDICES[edge_vert]];
}

// ─── Instance data lookup ──────────────────────────────────────────

fn get_size(i: u32) -> f32 {
    if i < 4u { return u.sizes_0_3[i]; }
    return u.extra.x;
}

fn get_angle(i: u32) -> f32 {
    if i < 4u { return u.angles_0_3[i]; }
    return u.extra.y;
}

// ─── Arbitrary-axis rotation (Rodrigues) ─────────────────────────

fn rotation_axis(axis: vec3<f32>, angle_deg: f32) -> mat3x3<f32> {
    let r = radians(angle_deg);
    let c = cos(r);
    let s = sin(r);
    let t = 1.0 - c;
    let a = normalize(axis);
    return mat3x3<f32>(
        vec3<f32>(t * a.x * a.x + c,       t * a.x * a.y + s * a.z, t * a.x * a.z - s * a.y),
        vec3<f32>(t * a.x * a.y - s * a.z,  t * a.y * a.y + c,       t * a.y * a.z + s * a.x),
        vec3<f32>(t * a.x * a.z + s * a.y,  t * a.y * a.z - s * a.x, t * a.z * a.z + c),
    );
}

/// Deterministic random rotation axis per face+instance.
fn face_axis(face: u32, iid: u32) -> vec3<f32> {
    let seed = face * 5u + iid + 100u;
    let ax = hash_f(seed * 13u + 7u);
    let ay = hash_f(seed * 17u + 11u);
    let az = hash_f(seed * 19u + 13u);
    // Ensure non-zero length
    let v = vec3<f32>(ax, ay, az);
    let len = length(v);
    if len < 0.001 { return vec3<f32>(0.0, 1.0, 0.0); }
    return v / len;
}

// ─── Scatter: per-face random displacement with slow drift ────────

fn scatter_offset(face: u32, iid: u32, time: f32, scatter: f32) -> vec3<f32> {
    if scatter <= 0.0 { return vec3<f32>(0.0); }

    let seed = face * 5u + iid;

    // Deterministic random direction per face+instance
    let rx = hash_f(seed * 3u + 0u);
    let ry = hash_f(seed * 3u + 1u);
    let rz = hash_f(seed * 3u + 2u);
    let base = vec3<f32>(rx, ry, rz) * 1.5;

    // Slow sinusoidal drift so scattered faces float around
    let phase = hash_f(seed * 7u) * 6.283;
    let freq = 0.3 + hash_f(seed * 11u + 1u) * 0.4;
    let drift = vec3<f32>(
        sin(time * freq + phase),
        cos(time * freq * 0.7 + phase * 1.3),
        sin(time * freq * 0.5 + phase * 0.7),
    ) * 0.3;

    return (base + drift) * scatter;
}

// ─── Transform: scatter → scale → rotate → project ───────────────

fn transform(pos: vec3<f32>, face: u32, iid: u32) -> vec4<f32> {
    let scatter = u.extra.w;
    let time = u.extra2.x;
    let envelope = u.extra2.y;
    let offset = scatter_offset(face, iid, time, scatter);
    let displaced = pos + offset;

    let size = get_size(iid);
    let angle = get_angle(iid);
    let scaled = displaced * size;

    // Base rotation: Y-axis keeps cube structure intact at rest
    let base = rotation_axis(vec3<f32>(0.0, 1.0, 0.0), angle);
    // Snap kick: per-face random axis, 360° scaled by envelope (decays to identity)
    let kick_axis = face_axis(face, iid);
    let kick = rotation_axis(kick_axis, envelope * 360.0);
    let rotated = kick * base * scaled;

    return u.view_proj * vec4<f32>(rotated, 1.0);
}

// ─── Pass 1: Triangle fill (36 verts) ─────────────────────────────

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    return transform(get_tri_vertex(vid), vid / 6u, iid);
}

// ─── Pass 2: Line edges (48 verts) ────────────────────────────────

@vertex
fn vs_edges(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    return transform(get_edge_vertex(vid), vid / 8u, iid);
}

// ─── Fragment shader (shared) ─────────────────────────────────────

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    let c = u.extra.z;
    return vec4<f32>(c, c, c, 1.0);
}

// ─── Pass 3: Intersection outlines ───────────────────────────────
// Re-rasterizes triangles, fragment shader compares its depth against
// the depth buffer from pass 1. Where another surface is very close,
// output a white intersection line.

@group(0) @binding(1) var depth_tex: texture_depth_2d;

struct IntersectOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_intersect(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> IntersectOut {
    var out: IntersectOut;
    out.position = transform(get_tri_vertex(vid), vid / 6u, iid);
    return out;
}

@fragment
fn fs_intersect(@builtin(position) frag_coord: vec4<f32>) -> @location(0) vec4<f32> {
    let my_depth = frag_coord.z;
    let px = vec2<i32>(i32(frag_coord.x), i32(frag_coord.y));
    let stored_depth = textureLoad(depth_tex, px, 0);

    // Positive = this fragment is behind the stored surface.
    // Pass 1's depth bias pushes stored values deeper, so self-fragments
    // always have behind < 0 and get discarded.
    let behind = my_depth - stored_depth;
    let threshold = 0.002;
    if behind < 0.0 || behind > threshold {
        discard;
    }

    // Intensity peaks at the intersection line and falls off
    let intensity = 1.0 - (behind / threshold);
    return vec4<f32>(intensity, intensity, intensity, intensity);
}
