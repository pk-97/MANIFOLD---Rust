// node.render_scene — shadow-map DEPTH pass. One draw per object per
// shadow-casting light: transform the object's geometry by that light's
// view-projection (see Light::shadow_view_proj — Sun ortho / Point
// single-face perspective) and write ONLY depth. No colour attachment;
// the fragment is void. The resulting Depth32Float map is sampled as a
// `texture_depth_2d` in the main lit pass via `textureSampleCompareLevel`
// (PCF).
//
// The `Vertex` layout is byte-identical to render_scene.wgsl (48 bytes)
// so the SAME per-object mesh vertex buffer binds here at @binding(1)
// with no re-pack. Only position is read; normal/uv are ignored (a
// depth pass has no shading).
//
// The shadow pass instances too (REALTIME_3D_DESIGN.md §10 D11+P8): the
// SAME per-object instances buffer (or identity stub) binds at
// @binding(2), and vs_main applies the identical instance-then-model TRS
// composition as render_scene.wgsl's vs_main, so a shadow-casting
// instance's silhouette in the depth map matches its silhouette in the
// lit pass exactly.

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

// 128 bytes: the light's view-projection and this object's model matrix.
// Bound per (caster, object) draw via GpuBinding::Bytes at @binding(0).
struct ShadowUniforms {
    light_view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> su: ShadowUniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
@group(0) @binding(2) var<storage, read> instances: array<Instance>;

// Bit-for-bit the same as render_scene.wgsl's euler_xyz — forked, not
// shared, per this file's header convention.
fn euler_xyz(angles: vec3<f32>) -> mat3x3<f32> {
    let cx = cos(angles.x);
    let sx = sin(angles.x);
    let cy = cos(angles.y);
    let sy = sin(angles.y);
    let cz = cos(angles.z);
    let sz = sin(angles.z);

    let rx = mat3x3<f32>(
        vec3<f32>(1.0, 0.0, 0.0),
        vec3<f32>(0.0, cx, sx),
        vec3<f32>(0.0, -sx, cx),
    );
    let ry = mat3x3<f32>(
        vec3<f32>(cy, 0.0, -sy),
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(sy, 0.0, cy),
    );
    let rz = mat3x3<f32>(
        vec3<f32>(cz, sz, 0.0),
        vec3<f32>(-sz, cz, 0.0),
        vec3<f32>(0.0, 0.0, 1.0),
    );
    return rz * ry * rx;
}

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    let v = verts[vid];
    let inst = instances[iid];
    let rot = euler_xyz(inst.rot_pad.xyz);
    let inst_pos = rot * (v.position * inst.pos_scale.w) + inst.pos_scale.xyz;
    let world = su.model * vec4<f32>(inst_pos, 1.0);
    return su.light_view_proj * world;
}

// Void fragment — writes no colour. Paired with a colourless render pass
// (GpuEncoder::draw_instanced_depth_only) so the only output is depth.
@fragment
fn fs_shadow() {}
