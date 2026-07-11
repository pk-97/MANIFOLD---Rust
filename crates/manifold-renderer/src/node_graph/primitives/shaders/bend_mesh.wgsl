// node.bend_mesh — HAND parity oracle for bend_mesh_body.wgsl. Rotation
// convention: axis=X rotates (x,y) about Z pivoting x by center; axis=Y
// rotates (y,z) about X pivoting y by center; axis=Z rotates (z,x) about Y
// pivoting z by center. theta = angle * (coord - center) * w. Position AND
// normal rotate by the same theta (position pivoted by center, normal not —
// D4 exact). Uniform layout/bindings match the generated standalone kernel
// (axis/angle/center params, then the derived weights_len, dispatch_count,
// pad) so the gpu_tests parity oracle packs ONE uniform for both kernels.

struct Uniforms {
    axis: u32,
    angle: f32,
    center: f32,
    weights_len: u32,
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
    let s = coord - u.center;
    let theta = u.angle * s * w;
    let c = cos(theta);
    let sn = sin(theta);

    var pos = v.position;
    var nrm = v.normal;
    if u.axis == 0u {
        let py = pos.y;
        pos.x = u.center + s * c - py * sn;
        pos.y = s * sn + py * c;
        let nx = nrm.x;
        let ny = nrm.y;
        nrm.x = nx * c - ny * sn;
        nrm.y = nx * sn + ny * c;
    } else if u.axis == 1u {
        let pz = pos.z;
        pos.y = u.center + s * c - pz * sn;
        pos.z = s * sn + pz * c;
        let ny = nrm.y;
        let nz = nrm.z;
        nrm.y = ny * c - nz * sn;
        nrm.z = ny * sn + nz * c;
    } else {
        let px = pos.x;
        pos.z = u.center + s * c - px * sn;
        pos.x = s * sn + px * c;
        let nz = nrm.z;
        let nx = nrm.x;
        nrm.z = nz * c - nx * sn;
        nrm.x = nz * sn + nx * c;
    }

    dst[idx].position = pos;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = nrm;
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = v.uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
