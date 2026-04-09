// Nested Cubes — instanced gap-face cubes with EMA-smoothed Y-axis rotation.
//
// Geometry: 6 unwelded quads scaled 0.5 from their face centers (Primitive SOP).
// 5 instances with ramp scaling (1.0 → 2.0) and lagged rotation.
// Two-pass rendering: pass 1 = solid black occluders, pass 2 = white wireframe.

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

// ─── Gap-face cube geometry ────────────────────────────────────────
// 6 faces × 6 vertices = 36 total. Each face is a quad scaled 0.5
// from its geometric center, creating gaps between faces.
// Vertex positions: ±0.25 on the two free axes, ±0.5 on the face axis.

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

fn get_vertex(vid: u32) -> vec3<f32> {
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

// ─── Vertex shader ─────────────────────────────────────────────────

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    let pos = get_vertex(vid);
    let size = get_size(iid);
    let angle = get_angle(iid);

    let scaled = pos * size;
    let rotated = rotation_y(angle) * scaled;

    return u.view_proj * vec4<f32>(rotated, 1.0);
}

// ─── Fragment shader ───────────────────────────────────────────────

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    let c = u.extra.z;
    return vec4<f32>(c, c, c, 1.0);
}
