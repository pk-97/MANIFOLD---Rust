// node.platonic_solid_mesh — CPU-origin compact source upload.
//
// UploadVertex is deliberately 32 bytes (position + normal). A full
// MeshVertex is 64 bytes, which would exceed Metal's setBytes inline limit at
// the fixed 108-vertex capacity. The bridge restores the zero UV/tangent
// fields while applying the scalar radius.

const PLATONIC_MESH_CAPACITY: u32 = 108u;

struct UploadVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
    tangent: vec4<f32>,
};

struct Uniforms {
    count: u32,
    capacity: u32,
    radius: f32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<uniform> src: array<UploadVertex, PLATONIC_MESH_CAPACITY>;
@group(0) @binding(2) var<storage, read_write> out_vertices: array<MeshVertex>;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= u.capacity {
        return;
    }
    if i < u.count && i < PLATONIC_MESH_CAPACITY {
        let source = src[i];
        out_vertices[i] = MeshVertex(
            source.position * u.radius,
            0.0,
            source.normal,
            0.0,
            vec2<f32>(0.0, 0.0),
            vec2<f32>(0.0, 0.0),
            vec4<f32>(0.0),
        );
    } else {
        // Zero-position triangles are degenerate padding. Keep every field
        // zero so stale data from an earlier, larger shape cannot leak.
        out_vertices[i] = MeshVertex(
            vec3<f32>(0.0),
            0.0,
            vec3<f32>(0.0),
            0.0,
            vec2<f32>(0.0),
            vec2<f32>(0.0),
            vec4<f32>(0.0),
        );
    }
}
