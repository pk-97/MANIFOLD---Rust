// node.generate_cube_mesh — emit a unit cube as 36 triangle-list
// MeshVertex entries (6 faces × 2 triangles × 3 vertices) with
// per-face outward normals. Vertex data ported from
// generators/shaders/digital_plants_render.wgsl's hardcoded
// CUBE_POSITIONS + CUBE_NORMALS constants.
//
// One thread emits one vertex.

struct CubeUniforms {
    capacity: u32,
    size: f32,
    _pad0: u32,
    _pad1: u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: CubeUniforms;
@group(0) @binding(1) var<storage, read_write> dst: array<MeshVertex>;

const CUBE_POSITIONS: array<vec3<f32>, 36> = array<vec3<f32>, 36>(
    // Front face (+Z)
    vec3(-0.5, -0.5,  0.5), vec3( 0.5, -0.5,  0.5), vec3( 0.5,  0.5,  0.5),
    vec3(-0.5, -0.5,  0.5), vec3( 0.5,  0.5,  0.5), vec3(-0.5,  0.5,  0.5),
    // Back face (-Z)
    vec3( 0.5, -0.5, -0.5), vec3(-0.5, -0.5, -0.5), vec3(-0.5,  0.5, -0.5),
    vec3( 0.5, -0.5, -0.5), vec3(-0.5,  0.5, -0.5), vec3( 0.5,  0.5, -0.5),
    // Right face (+X)
    vec3( 0.5, -0.5,  0.5), vec3( 0.5, -0.5, -0.5), vec3( 0.5,  0.5, -0.5),
    vec3( 0.5, -0.5,  0.5), vec3( 0.5,  0.5, -0.5), vec3( 0.5,  0.5,  0.5),
    // Left face (-X)
    vec3(-0.5, -0.5, -0.5), vec3(-0.5, -0.5,  0.5), vec3(-0.5,  0.5,  0.5),
    vec3(-0.5, -0.5, -0.5), vec3(-0.5,  0.5,  0.5), vec3(-0.5,  0.5, -0.5),
    // Top face (+Y)
    vec3(-0.5,  0.5,  0.5), vec3( 0.5,  0.5,  0.5), vec3( 0.5,  0.5, -0.5),
    vec3(-0.5,  0.5,  0.5), vec3( 0.5,  0.5, -0.5), vec3(-0.5,  0.5, -0.5),
    // Bottom face (-Y)
    vec3(-0.5, -0.5, -0.5), vec3( 0.5, -0.5, -0.5), vec3( 0.5, -0.5,  0.5),
    vec3(-0.5, -0.5, -0.5), vec3( 0.5, -0.5,  0.5), vec3(-0.5, -0.5,  0.5),
);

const CUBE_NORMALS: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3( 0.0,  0.0,  1.0),  // Front
    vec3( 0.0,  0.0, -1.0),  // Back
    vec3( 1.0,  0.0,  0.0),  // Right
    vec3(-1.0,  0.0,  0.0),  // Left
    vec3( 0.0,  1.0,  0.0),  // Top
    vec3( 0.0, -1.0,  0.0),  // Bottom
);

// Per-face UV from the unit-cube position, following the conventional
// cubemap cross unwrap:
//   face 0 (+Z): uv = (x_norm,     1 - y_norm)
//   face 1 (-Z): uv = (1 - x_norm, 1 - y_norm)
//   face 2 (+X): uv = (1 - z_norm, 1 - y_norm)
//   face 3 (-X): uv = (z_norm,     1 - y_norm)
//   face 4 (+Y): uv = (x_norm,     z_norm)
//   face 5 (-Y): uv = (x_norm,     1 - z_norm)
// where *_norm = coord * 0.5 + 0.5 maps [-0.5, 0.5] → [0, 1].
fn cube_face_uv(face: u32, unit_pos: vec3<f32>) -> vec2<f32> {
    let n = unit_pos + vec3<f32>(0.5, 0.5, 0.5); // [0, 1]
    switch face {
        case 0u: { return vec2<f32>(n.x, 1.0 - n.y); }            // +Z
        case 1u: { return vec2<f32>(1.0 - n.x, 1.0 - n.y); }      // -Z
        case 2u: { return vec2<f32>(1.0 - n.z, 1.0 - n.y); }      // +X
        case 3u: { return vec2<f32>(n.z, 1.0 - n.y); }            // -X
        case 4u: { return vec2<f32>(n.x, n.z); }                  // +Y
        default: { return vec2<f32>(n.x, 1.0 - n.z); }            // -Y
    }
}

@compute @workgroup_size(64, 1, 1)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }
    if i >= 36u {
        dst[i].position = vec3<f32>(0.0, 0.0, 0.0);
        dst[i]._pad0 = 0.0;
        dst[i].normal = vec3<f32>(0.0, 1.0, 0.0);
        dst[i]._pad1 = 0.0;
        dst[i].uv = vec2<f32>(0.0, 0.0);
        dst[i]._pad2 = vec2<f32>(0.0, 0.0);
        return;
    }

    let face = i / 6u;
    let unit_pos = CUBE_POSITIONS[i];
    let pos = unit_pos * u.size;
    let normal = CUBE_NORMALS[face];
    let uv = cube_face_uv(face, unit_pos);

    dst[i].position = pos;
    dst[i]._pad0 = 0.0;
    dst[i].normal = normal;
    dst[i]._pad1 = 0.0;
    dst[i].uv = uv;
    dst[i]._pad2 = vec2<f32>(0.0, 0.0);
}
