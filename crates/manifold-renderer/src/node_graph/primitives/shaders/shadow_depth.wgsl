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

@group(0) @binding(0) var<uniform> su: ShadowUniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> @builtin(position) vec4<f32> {
    let v = verts[vid];
    let world = su.model * vec4<f32>(v.position, 1.0);
    return su.light_view_proj * world;
}

// Void fragment — writes no colour. Paired with a colourless render pass
// (GpuEncoder::draw_instanced_depth_only) so the only output is depth.
@fragment
fn fs_shadow() {}
