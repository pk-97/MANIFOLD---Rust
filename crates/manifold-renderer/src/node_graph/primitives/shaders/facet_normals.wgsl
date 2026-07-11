// node.facet_normals — HAND parity oracle for facet_normals_body.wgsl.
// Per-VERTEX flat normal on a flat triangle-list Array<MeshVertex>: thread idx
// reads its triangle's 3 verts (base = 3*(idx/3)), computes the cross-product
// normal, writes vertex idx (position + uv unchanged). Trailing partial triangle
// (base+2 >= dispatch_count) passes through unchanged. Uniform layout + bindings
// match the generated standalone kernel so the gpu_tests parity oracle packs one
// uniform for both.

struct Uniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
    let idx = gid.x;
    if idx >= u.dispatch_count { return; }

    let base = (idx / 3u) * 3u;
    let self_v = src[idx];

    if base + 2u < u.dispatch_count {
        let v0 = src[base].position;
        let v1 = src[base + 1u].position;
        let v2 = src[base + 2u].position;
        let n = normalize(cross(v1 - v0, v2 - v0));
        dst[idx].position = self_v.position;
        dst[idx]._pad0 = 0.0;
        dst[idx].normal = n;
        dst[idx]._pad1 = 0.0;
        dst[idx].uv = self_v.uv;
        dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
    } else {
        dst[idx] = self_v;
    }
}
