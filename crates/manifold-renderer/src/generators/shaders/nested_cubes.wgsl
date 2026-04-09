// Nested Cubes — instanced gap-face cubes with EMA-smoothed Y-axis rotation.
//
// Geometry: 6 unwelded quads scaled 0.5 from their face centers (Primitive SOP).
// 5 instances with ramp scaling (1.0 → 2.0) and lagged rotation.
// Two-pass rendering:
//   Pass 1 (vs_main): 36 triangle vertices — solid black occluders
//   Pass 2 (vs_edges): 48 line vertices — white quad outlines (no diagonals)

struct Uniforms {
    view_proj: mat4x4<f32>,
    // x: size[0], y: size[1], z: size[2], w: size[3]
    sizes_0_3: vec4<f32>,
    // x: angle[0], y: angle[1], z: angle[2], w: angle[3]
    angles_0_3: vec4<f32>,
    // x: size[4], y: angle[4], z: color (0=black, 1=white), w: unused
    extra: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// ─── Gap-face cube: triangle geometry (36 verts) ──────────────────
// 6 faces × 2 triangles × 3 vertices. Each face scaled 0.5 from center.
// Positions: ±0.25 on free axes, ±0.5 on face axis.

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
// 6 faces × 4 edges × 2 endpoints = 48 vertices.
// Each face has 4 corner vertices; edges connect them in order.
// Corners per face (BL, BR, TR, TL in the face's local 2D space):

const FACE_CORNERS: array<array<vec3<f32>, 4>, 6> = array<array<vec3<f32>, 4>, 6>(
    // Front (+Z)
    array<vec3<f32>, 4>(
        vec3<f32>(-0.25, -0.25,  0.5),
        vec3<f32>( 0.25, -0.25,  0.5),
        vec3<f32>( 0.25,  0.25,  0.5),
        vec3<f32>(-0.25,  0.25,  0.5),
    ),
    // Back (-Z)
    array<vec3<f32>, 4>(
        vec3<f32>( 0.25, -0.25, -0.5),
        vec3<f32>(-0.25, -0.25, -0.5),
        vec3<f32>(-0.25,  0.25, -0.5),
        vec3<f32>( 0.25,  0.25, -0.5),
    ),
    // Right (+X)
    array<vec3<f32>, 4>(
        vec3<f32>( 0.5, -0.25,  0.25),
        vec3<f32>( 0.5, -0.25, -0.25),
        vec3<f32>( 0.5,  0.25, -0.25),
        vec3<f32>( 0.5,  0.25,  0.25),
    ),
    // Left (-X)
    array<vec3<f32>, 4>(
        vec3<f32>(-0.5, -0.25, -0.25),
        vec3<f32>(-0.5, -0.25,  0.25),
        vec3<f32>(-0.5,  0.25,  0.25),
        vec3<f32>(-0.5,  0.25, -0.25),
    ),
    // Top (+Y)
    array<vec3<f32>, 4>(
        vec3<f32>(-0.25,  0.5,  0.25),
        vec3<f32>( 0.25,  0.5,  0.25),
        vec3<f32>( 0.25,  0.5, -0.25),
        vec3<f32>(-0.25,  0.5, -0.25),
    ),
    // Bottom (-Y)
    array<vec3<f32>, 4>(
        vec3<f32>(-0.25, -0.5, -0.25),
        vec3<f32>( 0.25, -0.5, -0.25),
        vec3<f32>( 0.25, -0.5,  0.25),
        vec3<f32>(-0.25, -0.5,  0.25),
    ),
);

// Edge indices within each face: (0→1, 1→2, 2→3, 3→0)
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

// ─── Y-axis rotation matrix ───────────────────────────────────────

fn rotation_y(angle_deg: f32) -> mat3x3<f32> {
    let r = radians(angle_deg);
    let c = cos(r);
    let s = sin(r);
    return mat3x3<f32>(
        vec3<f32>( c, 0.0, s),
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(-s, 0.0, c),
    );
}

fn transform(pos: vec3<f32>, iid: u32) -> vec4<f32> {
    let size = get_size(iid);
    let angle = get_angle(iid);
    let scaled = pos * size;
    let rotated = rotation_y(angle) * scaled;
    return u.view_proj * vec4<f32>(rotated, 1.0);
}

// ─── Pass 1: Triangle fill (36 verts) ─────────────────────────────

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    return transform(get_tri_vertex(vid), iid);
}

// ─── Pass 2: Line edges (48 verts) ────────────────────────────────

@vertex
fn vs_edges(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    return transform(get_edge_vertex(vid), iid);
}

// ─── Fragment shader (shared) ─────────────────────────────────────

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    let c = u.extra.z;
    return vec4<f32>(c, c, c, 1.0);
}
