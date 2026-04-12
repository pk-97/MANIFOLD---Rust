// Digital Plants — Shadow pass: depth-only instanced cubes from light POV.

struct ShadowUniforms {
    light_view_proj: mat4x4<f32>,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: ShadowUniforms;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;

// ─── Cube geometry (36 vertices = 6 faces x 2 triangles x 3 verts) ──

const CUBE_VERTS = array<vec3<f32>, 36>(
    // Front face (+Z)
    vec3(-0.5, -0.5,  0.5), vec3( 0.5, -0.5,  0.5), vec3( 0.5,  0.5,  0.5),
    vec3(-0.5, -0.5,  0.5), vec3( 0.5,  0.5,  0.5), vec3(-0.5,  0.5,  0.5),
    // Back face (-Z)
    vec3( 0.5, -0.5, -0.5), vec3(-0.5, -0.5, -0.5), vec3(-0.5,  0.5, -0.5),
    vec3( 0.5, -0.5, -0.5), vec3(-0.5,  0.5, -0.5), vec3( 0.5,  0.5, -0.5),
    // Right face (+X)
    vec3( 0.5, -0.5,  0.5), vec3( 0.5, -0.5, -0.5), vec3( 0.5,  0.5, -0.5),
    vec3( 0.5, -0.5,  0.5), vec3( 0.5,  0.5, -0.5), vec3( 0.5,  0.5,  0.5),
    // Left face (-X)
    vec3(-0.5, -0.5, -0.5), vec3(-0.5, -0.5,  0.5), vec3(-0.5,  0.5,  0.5),
    vec3(-0.5, -0.5, -0.5), vec3(-0.5,  0.5,  0.5), vec3(-0.5,  0.5, -0.5),
    // Top face (+Y)
    vec3(-0.5,  0.5,  0.5), vec3( 0.5,  0.5,  0.5), vec3( 0.5,  0.5, -0.5),
    vec3(-0.5,  0.5,  0.5), vec3( 0.5,  0.5, -0.5), vec3(-0.5,  0.5, -0.5),
    // Bottom face (-Y)
    vec3(-0.5, -0.5, -0.5), vec3( 0.5, -0.5, -0.5), vec3( 0.5, -0.5,  0.5),
    vec3(-0.5, -0.5, -0.5), vec3( 0.5, -0.5,  0.5), vec3(-0.5, -0.5,  0.5),
);

// ─── Rotation matrix from Euler angles (XYZ order) ─────────────────

fn rotation_matrix(angles: vec3<f32>) -> mat3x3<f32> {
    let cx = cos(angles.x); let sx = sin(angles.x);
    let cy = cos(angles.y); let sy = sin(angles.y);
    let cz = cos(angles.z); let sz = sin(angles.z);
    return mat3x3<f32>(
        vec3( cy * cz,  sx * sy * cz - cx * sz,  cx * sy * cz + sx * sz),
        vec3( cy * sz,  sx * sy * sz + cx * cz,  cx * sy * sz - sx * cz),
        vec3(-sy,       sx * cy,                  cx * cy),
    );
}

// ─── Vertex shader ──────────────────────────────────────────────────

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_shadow(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOut {
    let inst = instances[iid];
    let local_pos = CUBE_VERTS[vid] * inst.pos_scale.w;
    let rot = rotation_matrix(inst.rot_pad.xyz);
    let world_pos = rot * local_pos + inst.pos_scale.xyz;
    var out: VsOut;
    out.clip_pos = u.light_view_proj * vec4<f32>(world_pos, 1.0);
    return out;
}

// ─── Fragment shader (depth-only, color discarded) ──────────────────

@fragment
fn fs_shadow() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
