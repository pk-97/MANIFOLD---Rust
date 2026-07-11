// node.morph_mesh — HAND parity oracle for morph_mesh_body.wgsl. Static
// two-mesh lerp by index: pos = mix(a, b, t*w), normal = normalize(mix(a.n,
// b.n, t*w)), uv from `a`. Uniform layout and bindings match the generated
// standalone kernel (param t, then the derived weights_len, dispatch_count,
// pad) so the gpu_tests parity oracle packs ONE uniform for both kernels.
//   w = weights[idx] if idx < weights_len else 1.0 (degrade, never silent 0)

struct Uniforms {
    t: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
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
@group(0) @binding(1) var<storage, read> a: array<MeshVertex>;
@group(0) @binding(2) var<storage, read> b: array<MeshVertex>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<MeshVertex>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.dispatch_count { return; }

    let va = a[idx];
    let vb = b[idx];
    let w = select(1.0, weights[idx], idx < u.weights_len);
    let tw = u.t * w;

    let pos = mix(va.position, vb.position, tw);
    let n = mix(va.normal, vb.normal, tw);
    let mag = max(length(n), 1e-12);

    dst[idx].position = pos;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = n / mag;
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = va.uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
