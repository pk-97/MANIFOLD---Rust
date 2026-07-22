// RT P0 prototype — G-buffer raster pass. Harness-authored (not part of the
// frozen rt_trace.metal kernel). Vertex-pulls from struct-of-arrays buffers
// (position/normal/material_id), indexed by [[vertex_id]], so no
// MTLVertexDescriptor / interleaved-stride bookkeeping is needed.
#include <metal_stdlib>
using namespace metal;

// packed_float3 everywhere in buffer-visible structs: bare MSL float3 is
// sizeof 16 and would desync from the repr(C) Rust mirrors (types.rs).
struct CameraUniforms {
    float4x4      view_proj;
    packed_float3 cam_pos; float _pad0;
};

struct GMaterial {         // mirrors types.rs GpuMaterial exactly (48 B)
    packed_float3 albedo;   float _p0;
    float  metallic; float roughness; float2 _p1;
    packed_float3 emissive; float _p2;
};

struct VOut {
    float4 clip_pos [[position]];
    float3 world_pos;
    float3 normal;
    uint   material_id [[flat]];
};

vertex VOut vs_gbuffer(
    uint                     vid          [[vertex_id]],
    constant packed_float3*  positions    [[buffer(0)]],
    constant packed_float3*  normals      [[buffer(1)]],
    constant uint*           material_ids [[buffer(2)]],
    constant CameraUniforms& cam          [[buffer(3)]])
{
    VOut out;
    float3 wp = positions[vid];
    out.world_pos = wp;
    out.normal = normals[vid];
    out.material_id = material_ids[vid];
    out.clip_pos = cam.view_proj * float4(wp, 1.0);
    return out;
}

struct GOut {
    float4 g_wpos [[color(0)]];  // rgba32f: xyz world pos, w = view distance
    float4 g_nrm  [[color(1)]];  // rgba16f: xyz world normal
    float4 g_alb  [[color(2)]];  // rgba16f: linear albedo
    float4 g_mat  [[color(3)]];  // rg16f: metallic, roughness
};

fragment GOut fs_gbuffer(
    VOut in [[stage_in]],
    constant GMaterial*      materials [[buffer(0)]],
    constant CameraUniforms& cam       [[buffer(1)]])
{
    GOut out;
    GMaterial m = materials[in.material_id];
    float view_dist = length(float3(cam.cam_pos) - in.world_pos);
    out.g_wpos = float4(in.world_pos, view_dist);
    out.g_nrm  = float4(normalize(in.normal), 0.0);
    out.g_alb  = float4(float3(m.albedo), 1.0);
    out.g_mat  = float4(m.metallic, m.roughness, 0.0, 0.0);
    return out;
}
