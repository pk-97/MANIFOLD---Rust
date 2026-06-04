// node.generate_cube_mesh — fusable BUFFER body (freeze §12, buffer domain),
// SOURCE. Emit a unit cube as 36 triangle-list MeshVertex entries (6 faces × 2
// tris × 3 verts) with per-face outward normals + cubemap-cross UVs. Matches
// generate_cube_mesh.wgsl bit-for-bit (same const tables + uv helper, inlined).
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the MeshVertex written to
// buf_vertices[idx]. The codegen synthesizes
//   struct Element { position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32> }
// from MeshVertex's Channels signature. `dispatch_count` (= capacity) is the
// wrapper guard; slots idx >= 36 are the padding vertices the hand writes as the
// degenerate (pos 0, normal +Y, uv 0). `max_capacity` is an allocation-only
// param the shader ignores (DCE drops it). Helper + consts are prefixed (gcm_)
// to stay collision-safe under future multi-atom fusion.
const GCM_CUBE_POSITIONS: array<vec3<f32>, 36> = array<vec3<f32>, 36>(
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

const GCM_CUBE_NORMALS: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3( 0.0,  0.0,  1.0),  // Front
    vec3( 0.0,  0.0, -1.0),  // Back
    vec3( 1.0,  0.0,  0.0),  // Right
    vec3(-1.0,  0.0,  0.0),  // Left
    vec3( 0.0,  1.0,  0.0),  // Top
    vec3( 0.0, -1.0,  0.0),  // Bottom
);

fn gcm_cube_face_uv(face: u32, unit_pos: vec3<f32>) -> vec2<f32> {
    let n = unit_pos + vec3<f32>(0.5, 0.5, 0.5); // [0, 1]
    switch face {
        case 0u: { return vec2<f32>(n.x, 1.0 - n.y); }       // +Z
        case 1u: { return vec2<f32>(1.0 - n.x, 1.0 - n.y); } // -Z
        case 2u: { return vec2<f32>(1.0 - n.z, 1.0 - n.y); } // +X
        case 3u: { return vec2<f32>(n.z, 1.0 - n.y); }       // -X
        case 4u: { return vec2<f32>(n.x, n.z); }             // +Y
        default: { return vec2<f32>(n.x, 1.0 - n.z); }       // -Y
    }
}

fn body(idx: u32, count: u32, max_capacity: i32, size: f32) -> Element {
    if idx >= 36u {
        // Padding vertex — degenerate (matches the hand kernel).
        return Element(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0, 0.0));
    }

    let face = idx / 6u;
    let unit_pos = GCM_CUBE_POSITIONS[idx];
    let pos = unit_pos * size;
    let normal = GCM_CUBE_NORMALS[face];
    let uv = gcm_cube_face_uv(face, unit_pos);

    return Element(pos, normal, uv);
}
