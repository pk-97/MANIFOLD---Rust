// node.render_instanced_3d_mesh — instanced triangle-list mesh
// rendering with a per-MaterialKind fragment shader. Sibling to
// render_3d_mesh.wgsl: same per-kind dispatch, same Uniforms shape,
// but the vertex shader applies a per-instance pos/scale/Euler
// transform.
//
// Vertex storage: Array<MeshVertex>, indexed by vertex_index.
// Instance storage: Array<InstanceTransform>, indexed by instance_index.
//
// Entry points mirror render_3d_mesh.wgsl exactly.

const PI: f32 = 3.14159265358979;

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

// Superset uniform — must match render_3d_mesh.wgsl's Uniforms layout
// exactly so the host can share the Rust struct between the two
// renderers. 16-byte aligned, 192 bytes.
struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,
    light_color: vec4<f32>,
    base_color: vec4<f32>,
    emission: vec4<f32>,
    pbr_metallic_roughness: vec4<f32>,
    specular: vec4<f32>,
    cel_params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
@group(0) @binding(2) var<storage, read> instances: array<Instance>;
@group(0) @binding(3) var envmap: texture_2d<f32>;
@group(0) @binding(4) var envmap_sampler: sampler;

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

// ===== Material kind fragment entry points (identical math to
// render_3d_mesh.wgsl; the only difference between the two files is
// the vertex shader's per-instance transform) =====

@fragment
fn fs_unlit(in: VsOut) -> @location(0) vec4<f32> {
    let rgb = u.base_color.rgb + u.emission.rgb;
    return vec4<f32>(rgb, u.base_color.a);
}

@fragment
fn fs_phong(in: VsOut) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let L = normalize(u.light_dir.xyz);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    let H = normalize(L + V);
    let n_dot_l = max(dot(N, L), 0.0);
    let n_dot_h = max(dot(N, H), 0.0);
    let ambient = u.light_color.a;
    let diffuse = u.base_color.rgb * (1.0 - ambient) * n_dot_l + u.base_color.rgb * ambient;
    let spec = u.specular.rgb * pow(n_dot_h, max(u.specular.w, 1.0)) * n_dot_l;
    let lit = (diffuse + spec) * u.light_color.rgb * u.light_dir.w;
    return vec4<f32>(lit + u.emission.rgb, u.base_color.a);
}

@fragment
fn fs_pbr(in: VsOut) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let L = normalize(u.light_dir.xyz);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    let H = normalize(L + V);
    let metallic = clamp(u.pbr_metallic_roughness.x, 0.0, 1.0);
    let roughness = max(u.pbr_metallic_roughness.y, 0.01);

    let n_dot_l = max(dot(N, L), 0.0);
    let n_dot_v = max(dot(N, V), 0.001);
    let n_dot_h = max(dot(N, H), 0.0);
    let v_dot_h = max(dot(V, H), 0.001);

    let F0 = mix(vec3<f32>(0.04), u.base_color.rgb, metallic);
    let a = roughness * roughness;
    let a2 = a * a;
    let denom_d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    let D = a2 / (PI * denom_d * denom_d);

    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let g_v = n_dot_v / (n_dot_v * (1.0 - k) + k);
    let g_l = n_dot_l / (n_dot_l * (1.0 - k) + k);
    let G = g_v * g_l;

    let F = F0 + (1.0 - F0) * pow(clamp(1.0 - v_dot_h, 0.0, 1.0), 5.0);
    let specular = (D * G * F) / (4.0 * n_dot_v * n_dot_l + 0.0001);
    let kd = (1.0 - F) * (1.0 - metallic);
    let diffuse = kd * u.base_color.rgb / PI;

    let direct = (diffuse + specular) * u.light_color.rgb * n_dot_l * u.light_dir.w;

    let R = reflect(-V, N);
    let azimuth = atan2(R.z, R.x);
    let elevation = asin(clamp(R.y, -1.0, 1.0));
    let uv = vec2<f32>(azimuth / (2.0 * PI) + 0.5, elevation / PI + 0.5);
    let ibl_sample = textureSampleLevel(envmap, envmap_sampler, uv, 0.0).rgb;
    let ibl_strength = 1.0 - roughness * 0.7;
    let ibl = F * ibl_sample * ibl_strength;

    let ambient = u.base_color.rgb * u.light_color.a;
    let rgb = direct + ibl + ambient + u.emission.rgb;
    return vec4<f32>(rgb, u.base_color.a);
}

@fragment
fn fs_cel(in: VsOut) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let L = normalize(u.light_dir.xyz);
    let n_dot_l = max(dot(N, L), 0.0);
    let bands = max(u.cel_params.x, 2.0);
    let band_low = u.cel_params.y;
    let band_high = u.cel_params.z;
    let snapped = floor(n_dot_l * bands) / (bands - 1.0);
    let level = mix(band_low, band_high, clamp(snapped, 0.0, 1.0));
    let lit = u.base_color.rgb * level * u.light_color.rgb * u.light_dir.w;
    return vec4<f32>(lit + u.emission.rgb, u.base_color.a);
}
