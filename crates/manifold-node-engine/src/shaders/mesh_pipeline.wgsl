// MeshPipeline — instanced 3D mesh rendering with depth testing and lighting.
//
// Vertex shader reads per-instance data from a storage buffer and transforms
// cube vertices by instance rotation + translation. Fragment shader computes
// per-pixel normals and simple two-point lighting.
//
// Storage buffer layout per instance:
//   position: vec3<f32>   (world-space)
//   scale:    f32
//   rotation: vec3<f32>   (Euler angles in radians)
//   _pad:     f32

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light0_pos: vec4<f32>,
    light1_pos: vec4<f32>,
    light0_color: vec4<f32>,
    light1_color: vec4<f32>,
    ambient_color: vec4<f32>,
    // x: metallic, y: roughness, z: instance_count, w: unused
    material: vec4<f32>,
};

struct Instance {
    // vec4(position.xyz, scale)
    pos_scale: vec4<f32>,
    // vec4(rotation.xyz, _pad)
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;

// Cube geometry: 36 vertices (12 triangles, 6 faces).
// Each vertex is encoded as position + normal in a const array.
// Positions are in [-0.5, 0.5] range.

struct CubeVertex {
    pos: vec3<f32>,
    normal: vec3<f32>,
};

// Front face (+Z)
const CUBE_FRONT: array<CubeVertex, 6> = array<CubeVertex, 6>(
    CubeVertex(vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>(0.0, 0.0, 1.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(0.0, 0.0, 1.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>(0.0, 0.0, 1.0)),
    CubeVertex(vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>(0.0, 0.0, 1.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>(0.0, 0.0, 1.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(0.0, 0.0, 1.0)),
);
// Back face (-Z)
const CUBE_BACK: array<CubeVertex, 6> = array<CubeVertex, 6>(
    CubeVertex(vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(0.0, 0.0, -1.0)),
    CubeVertex(vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(0.0, 0.0, -1.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5, -0.5), vec3<f32>(0.0, 0.0, -1.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(0.0, 0.0, -1.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5, -0.5), vec3<f32>(0.0, 0.0, -1.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(0.0, 0.0, -1.0)),
);
// Right face (+X)
const CUBE_RIGHT: array<CubeVertex, 6> = array<CubeVertex, 6>(
    CubeVertex(vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>(1.0, 0.0, 0.0)),
);
// Left face (-X)
const CUBE_LEFT: array<CubeVertex, 6> = array<CubeVertex, 6>(
    CubeVertex(vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>(-1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(-1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(-1.0, 0.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5, -0.5), vec3<f32>(-1.0, 0.0, 0.0)),
);
// Top face (+Y)
const CUBE_TOP: array<CubeVertex, 6> = array<CubeVertex, 6>(
    CubeVertex(vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(0.0, 1.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>(0.0, 1.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(0.0, 1.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(0.0, 1.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(0.0, 1.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5,  0.5, -0.5), vec3<f32>(0.0, 1.0, 0.0)),
);
// Bottom face (-Y)
const CUBE_BOTTOM: array<CubeVertex, 6> = array<CubeVertex, 6>(
    CubeVertex(vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(0.0, -1.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(0.0, -1.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(0.0, -1.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(0.0, -1.0, 0.0)),
    CubeVertex(vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(0.0, -1.0, 0.0)),
    CubeVertex(vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>(0.0, -1.0, 0.0)),
);

fn get_cube_vertex(vid: u32) -> CubeVertex {
    let face = vid / 6u;
    let idx = vid % 6u;
    switch face {
        case 0u: { return CUBE_FRONT[idx]; }
        case 1u: { return CUBE_BACK[idx]; }
        case 2u: { return CUBE_RIGHT[idx]; }
        case 3u: { return CUBE_LEFT[idx]; }
        case 4u: { return CUBE_TOP[idx]; }
        default: { return CUBE_BOTTOM[idx]; }
    }
}

// Build rotation matrix from Euler angles (XYZ order).
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

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VertexOutput {
    let inst = instances[iid];
    let cube = get_cube_vertex(vid);

    let pos = inst.pos_scale.xyz;
    let scale = inst.pos_scale.w;
    let rot = inst.rot_pad.xyz;

    let rot_mat = rotation_matrix(rot);
    let local_pos = cube.pos * scale;
    let world_pos = rot_mat * local_pos + pos;
    let world_normal = normalize(rot_mat * cube.normal);

    var out: VertexOutput;
    out.clip_pos = u.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.world_normal = world_normal;
    return out;
}

// ─── Fragment shader: two-point lighting ─────────────────────────────

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let V = normalize(u.camera_pos.xyz - in.world_pos);

    let roughness = u.material.y;
    let metallic = u.material.x;
    let base_color = vec3<f32>(0.15, 0.15, 0.15); // dark grey rock

    // Light 0
    let L0 = normalize(u.light0_pos.xyz - in.world_pos);
    let H0 = normalize(L0 + V);
    let diff0 = max(dot(N, L0), 0.0);
    let spec0 = pow(max(dot(N, H0), 0.0), mix(8.0, 128.0, 1.0 - roughness));
    let light0 = u.light0_color.rgb * u.light0_color.a
        * (base_color * diff0 + mix(vec3<f32>(0.04), base_color, metallic) * spec0);

    // Light 1
    let L1 = normalize(u.light1_pos.xyz - in.world_pos);
    let H1 = normalize(L1 + V);
    let diff1 = max(dot(N, L1), 0.0);
    let spec1 = pow(max(dot(N, H1), 0.0), mix(8.0, 128.0, 1.0 - roughness));
    let light1 = u.light1_color.rgb * u.light1_color.a
        * (base_color * diff1 + mix(vec3<f32>(0.04), base_color, metallic) * spec1);

    let ambient = u.ambient_color.rgb * u.ambient_color.a * base_color;
    let color = ambient + light0 + light1;

    return vec4<f32>(color, 1.0);
}
