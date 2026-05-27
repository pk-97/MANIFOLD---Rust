// node.render_3d_mesh — vertex+fragment pipeline that draws an
// Array<MeshVertex> as a triangle list with depth testing and
// simple two-point lighting. Phase B of BUFFER_PORT_PLAN.
//
// MeshVertex layout (32 bytes):
//   position: vec3<f32> + pad
//   normal:   vec3<f32> + pad
//
// Topology: every 3 consecutive vertices form one triangle.
// The vertex shader looks up vertex `vertex_index` directly
// from the storage buffer — no vertex buffer binding.
//
// Three fragment entry points share the same vertex shader:
//   fs_main          — Lambert + ambient shaded color
//   fs_world_pos     — emit interpolated world position (G-buffer)
//   fs_world_normal  — emit interpolated world-space surface normal
//                      (G-buffer; normalised, signed). Runs alongside
//                      fs_main when downstream PBR atoms need per-pixel
//                      V/L from world coordinates — TouchDesigner /
//                      Blender style multi-pass aspect output.

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
};

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_dir: vec4<f32>,    // xyz: world-space direction toward light, w: intensity
    light_color: vec4<f32>,  // rgb: light color, a: ambient strength
    base_color: vec4<f32>,   // rgb: surface color, a: alpha
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let v = verts[vid];
    var out: VsOut;
    out.clip_pos = u.view_proj * vec4<f32>(v.position, 1.0);
    out.world_pos = v.position;
    out.world_normal = v.normal;
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

// G-buffer fragment: emit interpolated world position (XYZ, with W=1
// so downstream samplers can distinguish "geometry covered this pixel"
// from "background, alpha=0" via .a).
@fragment
fn fs_world_pos(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.world_pos, 1.0);
}

// G-buffer fragment: emit normalised world-space surface normal in [-1, 1].
// Renormalised here because triangle interpolation across edges produces
// non-unit vectors. Alpha = 1 to mark geometry coverage.
@fragment
fn fs_world_normal(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    return vec4<f32>(n, 1.0);
}
