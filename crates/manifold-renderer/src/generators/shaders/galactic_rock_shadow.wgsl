// Galactic Rock — Shadow map depth pass.
//
// Renders the scene from a light's perspective to produce a depth map.
// Minimal fragment shader — only depth matters.

struct ShadowUniforms {
    light_view_proj: mat4x4<f32>,
    _pad0: vec4<f32>,
    _pad1: vec4<f32>,
    _pad2: vec4<f32>,
    _pad3: vec4<f32>,
    _pad4: vec4<f32>,
    _pad5: vec4<f32>,
    _pad6: vec4<f32>,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: ShadowUniforms;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;

// ─── Cube geometry (same as mesh_pipeline.wgsl) ─────────────────────

struct CubeVertex {
    pos: vec3<f32>,
};

const CUBE_VERTS: array<vec3<f32>, 36> = array<vec3<f32>, 36>(
    // Front face (+Z)
    vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>( 0.5,  0.5,  0.5),
    vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>(-0.5,  0.5,  0.5),
    // Back face (-Z)
    vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-0.5,  0.5, -0.5),
    vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(-0.5,  0.5, -0.5), vec3<f32>( 0.5,  0.5, -0.5),
    // Right face (+X)
    vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>( 0.5,  0.5, -0.5),
    vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>( 0.5,  0.5,  0.5),
    // Left face (-X)
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>(-0.5,  0.5,  0.5),
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(-0.5,  0.5, -0.5),
    // Top face (+Y)
    vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>( 0.5,  0.5, -0.5),
    vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(-0.5,  0.5, -0.5),
    // Bottom face (-Y)
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>( 0.5, -0.5,  0.5),
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(-0.5, -0.5,  0.5),
);

fn rotation_matrix(angles: vec3<f32>) -> mat3x3<f32> {
    let cx = cos(angles.x); let sx = sin(angles.x);
    let cy = cos(angles.y); let sy = sin(angles.y);
    let cz = cos(angles.z); let sz = sin(angles.z);
    return mat3x3<f32>(
        vec3<f32>(cy * cz, cx * sz + sx * sy * cz, sx * sz - cx * sy * cz),
        vec3<f32>(-cy * sz, cx * cz - sx * sy * sz, sx * cz + cx * sy * sz),
        vec3<f32>(sy, -sx * cy, cx * cy),
    );
}

@vertex
fn vs_shadow(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> @builtin(position) vec4<f32> {
    let inst = instances[iid];
    let local_pos = CUBE_VERTS[vid] * inst.pos_scale.w;
    let rot_mat = rotation_matrix(inst.rot_pad.xyz);
    let world_pos = rot_mat * local_pos + inst.pos_scale.xyz;
    return u.light_view_proj * vec4<f32>(world_pos, 1.0);
}

@fragment
fn fs_shadow() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
