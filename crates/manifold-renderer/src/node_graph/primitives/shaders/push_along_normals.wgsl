// node.push_along_normals — pos += normal * amount * w * f
//   w = weights[idx] if idx < weights_len else 1.0 (weights optional,
//       degrades to 1.0 for absent/short buffers, never to 0)
//   f = (field sample.r - field_bias) if has_field else 1.0
// normal and uv pass through unchanged (D4 — approximate normal policy).

struct Uniforms {
    count:        u32,
    weights_len:  u32,
    has_field:    u32,
    amount:       f32,
    field_bias:   f32,
    _pad0:        u32,
    _pad1:        u32,
    _pad2:        u32,
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
@group(0) @binding(3) var field_tex: texture_2d<f32>;
@group(0) @binding(4) var field_sampler: sampler;
@group(0) @binding(5) var<storage, read_write> dst: array<MeshVertex>;

@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= u.count { return; }

    let v = src[idx];
    let w = select(1.0, weights[idx], idx < u.weights_len);

    let sample = textureSampleLevel(field_tex, field_sampler, v.uv, 0.0).r;
    let f = select(1.0, sample - u.field_bias, u.has_field == 1u);

    let displaced = v.position + v.normal * (u.amount * w * f);

    dst[idx].position = displaced;
    dst[idx]._pad0 = 0.0;
    dst[idx].normal = v.normal;
    dst[idx]._pad1 = 0.0;
    dst[idx].uv = v.uv;
    dst[idx]._pad2 = vec2<f32>(0.0, 0.0);
}
