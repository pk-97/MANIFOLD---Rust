// node.plane_mesh — fusable BUFFER body (freeze section 12, buffer domain),
// SOURCE. Emit a camera-facing quad in the XY plane facing +Z as 6
// triangle-list MeshVertex entries (2 triangles), spanning
// [-width/2, width/2] × [-height/2, height/2], normal (0, 0, 1). UVs follow
// the cube's +Z face convention: uv = (n.x, 1.0 - n.y) where n is the
// 0..1-normalized position — so a layer composite sampled onto the plane
// reads upright when viewed from +Z.
//
// ABI (buffer standalone codegen): no array inputs, so the body takes
// (idx, count, <params...>) and returns the MeshVertex written to
// buf_vertices[idx]. `max_capacity` is an allocation-only param the shader
// ignores (DCE drops it). `dispatch_count` (= output capacity) is the
// wrapper guard; slots idx >= 6 are the padding vertices written as the
// same degenerate form the cube writes (pos 0, normal +Y, uv 0). Helpers
// are prefixed (pm2t_ for the small helpers, pm2p_ for the corner tables)
// to stay collision-safe under future multi-atom fusion.
const PM2T_CORNER_POS: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3(-1.0, -1.0, 0.0), vec3( 1.0, -1.0, 0.0), vec3( 1.0,  1.0, 0.0),
    vec3(-1.0, -1.0, 0.0), vec3( 1.0,  1.0, 0.0), vec3(-1.0,  1.0, 0.0),
);

const PM2T_CORNER_UV: array<vec2<f32>, 6> = array<vec2<f32>, 6>(
    vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 0.0),
    vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 0.0),
);

fn body(idx: u32, count: u32, max_capacity: i32, width: f32, height: f32) -> Element {
    if idx >= 6u {
        // Padding vertex — degenerate (matches the hand kernel).
        return Element(vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0), vec2<f32>(0.0, 0.0), vec4<f32>(0.0));
    }

    let corner = PM2T_CORNER_POS[idx];
    let pos = vec3<f32>(corner.x * (width * 0.5), corner.y * (height * 0.5), 0.0);
    let normal = vec3<f32>(0.0, 0.0, 1.0);
    let uv = PM2T_CORNER_UV[idx];

    return Element(pos, normal, uv, vec4<f32>(0.0));
}