// node.taper_mesh — HAND parity oracle for taper_mesh_body.wgsl. s = mix(1,
// taper, clamp((coord-center)/length, 0, 1) * w); off-axis position
// components scale by s, off-axis normal components divide by s then the
// normal renormalizes (D4 exact for this transform). Uniform layout/
// bindings match the generated standalone kernel (axis/taper/center/length
// params — field named `length` here since this hand file is independent
// of the codegen RESERVED-word rename — then the derived weights_len,
// dispatch_count, pad) so the gpu_tests parity oracle packs ONE uniform
// (identical byte layout) for both kernels.

struct Uniforms {
    axis: u32,
    taper: f32,
    center: f32,
    length: f32,
    weights_len: u32,
    dispatch_count: u32,
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

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> src: array<MeshVertex>;
@group(0) @binding(2) var<storage, read> weights: array<f32>;
@group(0) @binding(3) var<storage, read_write> dst: array<MeshVertex>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.dispatch_count { return; }

    let v = src[idx];
    let w = select(1.0, weights[idx], idx < u.weights_len);

    var coord: f32;
    if u.axis == 0u {
        coord = v.position.x;
    } else if u.axis == 1u {
        coord = v.position.y;
    } else {
        coord = v.position.z;
    }
    let len_safe = max(u.length, 1e-6);
    let t = clamp((coord - u.center) / len_safe, 0.0, 1.0);
    let s = mix(1.0, u.taper, t * w);
    let denom = select(s, 1e-6, abs(s) < 1e-6);

    var pos = v.position;
    var nrm = v.normal;
    if u.axis == 0u {
        pos.y = pos.y * s;
        pos.z = pos.z * s;
        nrm.y = nrm.y / denom;
        nrm.z = nrm.z / denom;
    } else if u.axis == 1u {
        pos.z = pos.z * s;
        pos.x = pos.x * s;
        nrm.z = nrm.z / denom;
        nrm.x = nrm.x / denom;
    } else {
        pos.x = pos.x * s;
        pos.y = pos.y * s;
        nrm.x = nrm.x / denom;
        nrm.y = nrm.y / denom;
    }
    nrm = normalize(nrm);

    dst[idx].position = pos;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = nrm;
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = v.uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
