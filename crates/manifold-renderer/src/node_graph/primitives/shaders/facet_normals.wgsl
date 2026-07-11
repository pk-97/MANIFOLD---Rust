// node.facet_normals — exact per-triangle flat normals on a flat
// triangle-list Array<MeshVertex>. One thread per triangle; the trailing
// partial triangle (1 or 2 leftover verts, only possible on the LAST
// thread) passes through unchanged.

struct Uniforms {
    vertex_count:    u32,
    full_triangles:  u32,
    _pad0:           u32,
    _pad1:           u32,
};

struct MeshVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> src: array<MeshVertex>;
@group(0) @binding(2) var<storage, read_write> dst: array<MeshVertex>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let tid = gid.x;
    let base = tid * 3u;

    if tid < u.full_triangles {
        let v0 = src[base];
        let v1 = src[base + 1u];
        let v2 = src[base + 2u];
        let e1 = v1.position - v0.position;
        let e2 = v2.position - v0.position;
        let n = normalize(cross(e1, e2));

        dst[base] = MeshVertex(v0.position, 0.0, n, 0.0, v0.uv, vec2<f32>(0.0, 0.0));
        dst[base + 1u] = MeshVertex(v1.position, 0.0, n, 0.0, v1.uv, vec2<f32>(0.0, 0.0));
        dst[base + 2u] = MeshVertex(v2.position, 0.0, n, 0.0, v2.uv, vec2<f32>(0.0, 0.0));
    } else if base < u.vertex_count {
        dst[base] = src[base];
        if base + 1u < u.vertex_count {
            dst[base + 1u] = src[base + 1u];
        }
    }
}
