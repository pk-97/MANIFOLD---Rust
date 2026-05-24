// node.render_3d_mesh_pbr_ibl — vertex+fragment pipeline.
//
// Vertex: looks up Array<MeshVertex>, transforms via view_proj. Mesh
// positions come in pre-displaced (the upstream displace_mesh chain
// applied the height to Y), so the vertex shader doesn't need to
// re-sample.
//
// Fragment: per-pixel normal from finite differences on `material.r`
// (the height channel — matches the legacy MetallicGlass full-res
// reflection trick), Cook-Torrance BRDF for direct lighting,
// equirectangular env-map sample for IBL.
//
// Bindings:
//   @binding(0) uniforms (PbrRenderUniforms)
//   @binding(1) verts (storage<read> Array<Vertex>)
//   @binding(2) tex_material (packed RGBA: r = height, g = metallic_var, b = edge)
//   @binding(3) tex_material_sampler
//   @binding(4) tex_env (equirectangular HDR)
//   @binding(5) tex_env_sampler
//
// Prepended at pipeline-creation: pbr_brdf.wgsl (Cook-Torrance helpers,
// pbr_equirect_uv).

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
}

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_pos: vec4<f32>,    // xyz = world position, w = intensity
    light_color: vec4<f32>,  // rgb = tint, a = unused
    material: vec4<f32>,     // metallic, roughness, displacement, edge_roughness_mul
    base_color: vec4<f32>,
    grid_info: vec4<f32>,    // grid_size, texel_inv, aspect, unused
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
@group(0) @binding(2) var tex_material: texture_2d<f32>;
@group(0) @binding(3) var tex_material_sampler: sampler;
@group(0) @binding(4) var tex_env: texture_2d<f32>;
@group(0) @binding(5) var tex_env_sampler: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
}

// Compute UV from world position for an XZ-aligned grid (matches the
// shape generate_grid_mesh + displace_mesh produce). The grid spans
// [-aspect, +aspect] in X, [-1, +1] in Z, with UV.x linear in X and
// UV.y linear in Z.
fn uv_from_world(world_pos: vec3<f32>) -> vec2<f32> {
    let aspect = u.grid_info.z;
    let u_coord = (world_pos.x / aspect) * 0.5 + 0.5;
    let v_coord = world_pos.z * 0.5 + 0.5;
    return vec2<f32>(u_coord, v_coord);
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let v = verts[vid];
    var out: VsOut;
    out.clip_pos = u.view_proj * vec4<f32>(v.position, 1.0);
    out.world_pos = v.position;
    out.uv = uv_from_world(v.position);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let V = normalize(u.camera_pos.xyz - in.world_pos);

    let displacement = u.material.z;
    let proc = textureSampleLevel(tex_material, tex_material_sampler, in.uv, 0.0);

    let metallic = u.material.x;
    let base_roughness = max(u.material.y, 0.01);
    let edge_amount = proc.b;
    let edge_mul = u.material.w;
    let roughness = mix(base_roughness, min(base_roughness * edge_mul, 0.5), edge_amount);

    // Per-pixel normal from 4-tap finite differences on the height
    // channel (material.r) — matches the legacy MetallicGlass full-
    // resolution reflection trick.
    let texel = u.grid_info.y;
    let h_px = textureSampleLevel(tex_material, tex_material_sampler, in.uv + vec2<f32>(texel, 0.0), 0.0).r;
    let h_nx = textureSampleLevel(tex_material, tex_material_sampler, in.uv - vec2<f32>(texel, 0.0), 0.0).r;
    let h_py = textureSampleLevel(tex_material, tex_material_sampler, in.uv + vec2<f32>(0.0, texel), 0.0).r;
    let h_ny = textureSampleLevel(tex_material, tex_material_sampler, in.uv - vec2<f32>(0.0, texel), 0.0).r;

    let aspect = u.grid_info.z;
    let dx_world_x = 2.0 * aspect * texel;
    let dx_world_z = 2.0 * texel;
    let dh_x = (h_px - h_nx) * displacement;
    let dh_z = (h_py - h_ny) * displacement;
    let tangent_x = vec3<f32>(dx_world_x, dh_x, 0.0);
    let tangent_z = vec3<f32>(0.0, dh_z, dx_world_z);
    let N = normalize(cross(tangent_z, tangent_x));

    let F0 = mix(vec3<f32>(0.04), u.base_color.rgb, metallic);

    // Direct lighting (one positional light with inverse-square attenuation).
    let L = normalize(u.light_pos.xyz - in.world_pos);
    let NdotL = max(dot(N, L), 0.0);
    let NdotV = max(dot(N, V), 0.001);

    let light_dist = length(u.light_pos.xyz - in.world_pos);
    let attenuation = 1.0 / (1.0 + light_dist * light_dist / 25.0);

    let spec = pbr_cook_torrance_specular(N, L, V, roughness, F0);
    let H = normalize(L + V);
    let VdotH = max(dot(V, H), 0.001);
    let F = pbr_f_schlick(VdotH, F0);
    let kD = (1.0 - F) * (1.0 - metallic);
    let diffuse = kD * u.base_color.rgb / PBR_PI;

    let light_intensity = u.light_pos.w;
    let direct = (diffuse + spec) * u.light_color.rgb * light_intensity * NdotL * attenuation;

    // Environment IBL.
    let R = reflect(-V, N);
    let env_uv = pbr_equirect_uv(R);
    let env = textureSampleLevel(tex_env, tex_env_sampler, env_uv, 0.0).rgb;
    let F_env = pbr_f_schlick(NdotV, F0);
    let env_scale = 1.0 - roughness * 0.7;
    let ibl = env * F_env * env_scale;

    let color = direct + ibl;
    let mapped = color / (color + vec3<f32>(1.0));

    return vec4<f32>(mapped, 1.0);
}
