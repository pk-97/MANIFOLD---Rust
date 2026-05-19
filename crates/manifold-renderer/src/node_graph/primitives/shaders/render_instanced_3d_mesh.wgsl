// node.render_instanced_3d_mesh — instanced triangle-list mesh
// rendering. Each instance applies a per-instance position +
// scale + Euler rotation to the same base mesh vertices. Phase
// B of BUFFER_PORT_PLAN.
//
// Vertex storage: Array<MeshVertex>, indexed by vertex_index.
// Instance storage: Array<InstanceTransform>, indexed by
// instance_index.

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    base_color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
@group(0) @binding(2) var<storage, read> instances: array<Instance>;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
};

// Build a 3×3 rotation matrix from XYZ Euler angles (XYZ order).
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
) -> VsOut {
    let v = verts[vid];
    let inst = instances[iid];
    let rot = euler_xyz(inst.rot_pad.xyz);
    let local_pos = v.position * inst.pos_scale.w;
    let world_pos = rot * local_pos + inst.pos_scale.xyz;
    let world_normal = rot * v.normal;

    var out: VsOut;
    out.clip_pos = u.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.world_normal = world_normal;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let l = normalize(u.light_dir.xyz);
    let n_dot_l = max(dot(n, l), 0.0);

    let ambient = u.base_color.rgb * u.light_color.a;
    let diffuse = u.base_color.rgb * u.light_color.rgb * n_dot_l * u.light_dir.w;

    return vec4<f32>(ambient + diffuse, u.base_color.a);
}
