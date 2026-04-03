// Galactic Rock — Phase 2-3: Instanced rendering with PBR lighting + shadow maps.
//
// Vertex shader: per-instance rotation + translation of cube geometry.
// Fragment shader: two-point Blinn-Phong with PCF soft shadows.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light0_pos: vec4<f32>,
    light1_pos: vec4<f32>,
    light0_color: vec4<f32>,  // rgb + intensity in a
    light1_color: vec4<f32>,  // rgb + intensity in a
    ambient_color: vec4<f32>, // rgb + intensity in a
    // x: metallic, y: roughness, z: shadow_map_size, w: unused
    material: vec4<f32>,
};

// Shadow map uniforms — light view-projection matrices
struct ShadowMatrices {
    light0_vp: mat4x4<f32>,
    light1_vp: mat4x4<f32>,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<uniform> shadow_mats: ShadowMatrices;
@group(0) @binding(3) var shadow_map_0: texture_depth_2d;
@group(0) @binding(4) var shadow_map_1: texture_depth_2d;
@group(0) @binding(5) var shadow_sampler: sampler_comparison;

// ─── Cube geometry ──────────────────────────────────────────────────

struct CubeVert {
    pos: vec3<f32>,
    normal: vec3<f32>,
};

const CUBE_POSITIONS: array<vec3<f32>, 36> = array<vec3<f32>, 36>(
    // Front (+Z)
    vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>( 0.5,  0.5,  0.5),
    vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>(-0.5,  0.5,  0.5),
    // Back (-Z)
    vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-0.5,  0.5, -0.5),
    vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>(-0.5,  0.5, -0.5), vec3<f32>( 0.5,  0.5, -0.5),
    // Right (+X)
    vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>( 0.5,  0.5, -0.5),
    vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>( 0.5,  0.5,  0.5),
    // Left (-X)
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-0.5, -0.5,  0.5), vec3<f32>(-0.5,  0.5,  0.5),
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>(-0.5,  0.5, -0.5),
    // Top (+Y)
    vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>( 0.5,  0.5,  0.5), vec3<f32>( 0.5,  0.5, -0.5),
    vec3<f32>(-0.5,  0.5,  0.5), vec3<f32>( 0.5,  0.5, -0.5), vec3<f32>(-0.5,  0.5, -0.5),
    // Bottom (-Y)
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>( 0.5, -0.5, -0.5), vec3<f32>( 0.5, -0.5,  0.5),
    vec3<f32>(-0.5, -0.5, -0.5), vec3<f32>( 0.5, -0.5,  0.5), vec3<f32>(-0.5, -0.5,  0.5),
);

const CUBE_NORMALS: array<vec3<f32>, 6> = array<vec3<f32>, 6>(
    vec3<f32>( 0.0,  0.0,  1.0),  // Front
    vec3<f32>( 0.0,  0.0, -1.0),  // Back
    vec3<f32>( 1.0,  0.0,  0.0),  // Right
    vec3<f32>(-1.0,  0.0,  0.0),  // Left
    vec3<f32>( 0.0,  1.0,  0.0),  // Top
    vec3<f32>( 0.0, -1.0,  0.0),  // Bottom
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

// ─── Vertex shader ──────────────────────────────────────────────────

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
    let local_pos = CUBE_POSITIONS[vid] * inst.pos_scale.w;
    let face_normal = CUBE_NORMALS[vid / 6u];
    let rot_mat = rotation_matrix(inst.rot_pad.xyz);
    let world_pos = rot_mat * local_pos + inst.pos_scale.xyz;
    let world_normal = normalize(rot_mat * face_normal);

    var out: VertexOutput;
    out.clip_pos = u.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.world_normal = world_normal;
    return out;
}

// ─── PCF Shadow sampling ────────────────────────────────────────────

fn sample_shadow(shadow_map: texture_depth_2d, samp: sampler_comparison, light_vp: mat4x4<f32>, world_pos: vec3<f32>, texel_size: f32) -> f32 {
    let light_clip = light_vp * vec4<f32>(world_pos, 1.0);
    let light_ndc = light_clip.xyz / light_clip.w;

    // Map NDC [-1,1] to UV [0,1] (Metal depth is already [0,1])
    let shadow_uv = vec2<f32>(light_ndc.x * 0.5 + 0.5, -light_ndc.y * 0.5 + 0.5);
    let depth = light_ndc.z;

    // Out-of-bounds = fully lit
    if shadow_uv.x < 0.0 || shadow_uv.x > 1.0 || shadow_uv.y < 0.0 || shadow_uv.y > 1.0 {
        return 1.0;
    }

    // 5-tap PCF (center + 4 neighbors)
    let bias = 0.003;
    let d = depth - bias;
    var shadow = 0.0;
    shadow += textureSampleCompare(shadow_map, samp, shadow_uv, d);
    shadow += textureSampleCompare(shadow_map, samp, shadow_uv + vec2(-texel_size, 0.0), d);
    shadow += textureSampleCompare(shadow_map, samp, shadow_uv + vec2( texel_size, 0.0), d);
    shadow += textureSampleCompare(shadow_map, samp, shadow_uv + vec2(0.0, -texel_size), d);
    shadow += textureSampleCompare(shadow_map, samp, shadow_uv + vec2(0.0,  texel_size), d);
    return shadow / 5.0;
}

// ─── Fragment shader: PBR + shadows ─────────────────────────────────

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let V = normalize(u.camera_pos.xyz - in.world_pos);

    let roughness = u.material.y;
    let metallic = u.material.x;
    let texel_size = 1.0 / u.material.z; // shadow map texel size
    let base_color = vec3<f32>(0.12, 0.12, 0.12); // dark grey rock
    let f0 = mix(vec3<f32>(0.04), base_color, metallic);
    let spec_power = mix(8.0, 256.0, 1.0 - roughness);

    // Shadow attenuation
    let shadow0 = sample_shadow(shadow_map_0, shadow_sampler, shadow_mats.light0_vp, in.world_pos, texel_size);
    let shadow1 = sample_shadow(shadow_map_1, shadow_sampler, shadow_mats.light1_vp, in.world_pos, texel_size);

    // Light 0
    let L0 = normalize(u.light0_pos.xyz - in.world_pos);
    let H0 = normalize(L0 + V);
    let diff0 = max(dot(N, L0), 0.0);
    let spec0 = pow(max(dot(N, H0), 0.0), spec_power);
    let light0 = u.light0_color.rgb * u.light0_color.a * shadow0
        * (base_color * diff0 + f0 * spec0);

    // Light 1
    let L1 = normalize(u.light1_pos.xyz - in.world_pos);
    let H1 = normalize(L1 + V);
    let diff1 = max(dot(N, L1), 0.0);
    let spec1 = pow(max(dot(N, H1), 0.0), spec_power);
    let light1 = u.light1_color.rgb * u.light1_color.a * shadow1
        * (base_color * diff1 + f0 * spec1);

    let ambient = u.ambient_color.rgb * u.ambient_color.a * base_color;
    let color = ambient + light0 + light1;

    return vec4<f32>(color, 1.0);
}
