// Digital Plants — Main render: instanced cubes with cel shading + PCF shadow.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_pos: vec4<f32>,
    light_color: vec4<f32>,    // rgb + intensity in w
    shadow_info: vec4<f32>,    // x: shadow_map_size, yzw: unused
};

struct ShadowMatrix {
    light_vp: mat4x4<f32>,
};

struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> instances: array<Instance>;
@group(0) @binding(2) var<uniform> shadow_mat: ShadowMatrix;
@group(0) @binding(3) var shadow_map: texture_depth_2d;
@group(0) @binding(4) var shadow_sampler: sampler_comparison;

// ─── Cube geometry + normals ────────────────────────────────────────

const CUBE_POSITIONS = array<vec3<f32>, 36>(
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

const CUBE_NORMALS = array<vec3<f32>, 6>(
    vec3( 0.0,  0.0,  1.0),  // Front
    vec3( 0.0,  0.0, -1.0),  // Back
    vec3( 1.0,  0.0,  0.0),  // Right
    vec3(-1.0,  0.0,  0.0),  // Left
    vec3( 0.0,  1.0,  0.0),  // Top
    vec3( 0.0, -1.0,  0.0),  // Bottom
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

// ─── PCF shadow sampling (5-tap cross pattern) ──────────────────────

fn sample_shadow(
    world_pos: vec3<f32>,
    texel_size: f32,
) -> f32 {
    let light_clip = shadow_mat.light_vp * vec4<f32>(world_pos, 1.0);
    let ndc = light_clip.xyz / light_clip.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, -ndc.y * 0.5 + 0.5);

    // Out-of-bounds: fully lit
    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
        return 1.0;
    }

    let depth = ndc.z - 0.003; // bias
    var shadow = textureSampleCompare(shadow_map, shadow_sampler, uv, depth);
    shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + vec2( texel_size, 0.0), depth);
    shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + vec2(-texel_size, 0.0), depth);
    shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + vec2(0.0,  texel_size), depth);
    shadow += textureSampleCompare(shadow_map, shadow_sampler, uv + vec2(0.0, -texel_size), depth);
    return shadow / 5.0;
}

// ─── Vertex shader ──────────────────────────────────────────────────

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
};

@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOut {
    let inst = instances[iid];
    let pos = inst.pos_scale.xyz;
    let scale = inst.pos_scale.w;
    let rot = rotation_matrix(inst.rot_pad.xyz);

    let local_pos = CUBE_POSITIONS[vid] * scale;
    let face_normal = CUBE_NORMALS[vid / 6u];

    let world_pos = rot * local_pos + pos;
    let world_normal = normalize(rot * face_normal);

    var out: VsOut;
    out.clip_pos = u.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.world_normal = world_normal;
    return out;
}

// ─── Fragment shader: cel shading ───────────────────────────────────

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let L = normalize(u.light_pos.xyz - in.world_pos);

    // Shadow
    let texel_size = 1.0 / u.shadow_info.x;
    let shadow = sample_shadow(in.world_pos, texel_size);

    // Cel shading: quantize dot(N, L) into 4 discrete stepping bands.
    // Shadow applied separately (spec: quantize NdotL, not NdotL*shadow).
    let NdotL = max(dot(N, L), 0.0);

    var band: f32;
    if NdotL < 0.25 {
        band = 0.08;   // deep shadow
    } else if NdotL < 0.5 {
        band = 0.35;   // mid shadow
    } else if NdotL < 0.75 {
        band = 0.65;   // mid light
    } else {
        band = 1.0;    // full light
    }

    // Base color: plant green
    let base_color = vec3<f32>(0.36, 0.56, 0.24);

    let final_color = base_color * band * shadow;

    return vec4<f32>(final_color, 1.0);
}
