// node.morph_mesh — HAND parity oracle for morph_mesh_body.wgsl. Static
// two-mesh lerp by index: pos = mix(a, b, t*w), normal = normalize(mix(a.n,
// b.n, t*w)), uv from `a`. Uniform layout and bindings match the generated
// standalone kernel (param t, blend_frames, then the derived weights_len and
// dispatch_count) so the gpu_tests parity oracle packs ONE uniform for both kernels.
//   w = weights[idx] if idx < weights_len else 1.0 (degrade, never silent 0)

struct Uniforms {
    t: f32,
    blend_frames: u32,
    weights_len: u32,
    dispatch_count: u32,
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
    var w = 1.0;
    if idx < u.weights_len {
        w = weights[idx];
    }
    let raw_tw = u.t * w;

    if u.blend_frames == 0u {
        let pos = mix(va.position, vb.position, raw_tw);
        let n = mix(va.normal, vb.normal, raw_tw);
        let mag = max(length(n), 1e-12);

        dst[idx].position = pos;
        dst[idx]._pad0 = 0.0;
        dst[idx].normal = n / mag;
        dst[idx]._pad1 = 0.0;
        dst[idx].uv = va.uv;
        dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
        dst[idx].tangent = va.tangent;
        return;
    }

    let tw = clamp(raw_tw, 0.0, 1.0);
    if tw <= 0.0 {
        dst[idx] = va;
        return;
    }
    if tw >= 1.0 {
        dst[idx].position = vb.position;
        dst[idx]._pad0 = 0.0;
        dst[idx].normal = vb.normal;
        dst[idx]._pad1 = 0.0;
        dst[idx].uv = va.uv;
        dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
        dst[idx].tangent = vb.tangent;
        return;
    }

    let mixed_normal = mix(va.normal, vb.normal, tw);
    let normal_length = length(mixed_normal);
    var normal = vec3<f32>(0.0, 1.0, 0.0);
    if normal_length > 1e-12 {
        normal = mixed_normal / normal_length;
    } else {
        let input_length = length(va.normal);
        if input_length > 1e-12 {
            normal = va.normal / input_length;
        } else {
            let target_length = length(vb.normal);
            if target_length > 1e-12 {
                normal = vb.normal / target_length;
            }
        }
    }

    let mixed_tangent = mix(va.tangent.xyz, vb.tangent.xyz, tw);
    let tangent_length = length(mixed_tangent);
    var tangent_xyz = vec3<f32>(0.0, 0.0, 0.0);
    if tangent_length > 1e-12 {
        let unit_tangent = mixed_tangent / tangent_length;
        let orthogonal = unit_tangent - normal * dot(unit_tangent, normal);
        let orthogonal_length = length(orthogonal);
        if orthogonal_length > 1e-12 {
            tangent_xyz = orthogonal / orthogonal_length;
        }
    }
    // At the exact midpoint the input endpoint wins the tie.
    let handedness = select(va.tangent.w, vb.tangent.w, tw > 0.5);

    dst[idx].position = mix(va.position, vb.position, tw);
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = normal;
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = va.uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
    dst[idx].tangent = vec4<f32>(tangent_xyz, handedness);
}
