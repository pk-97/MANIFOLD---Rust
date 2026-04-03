// Metallic Glass — Pass 5: Displaced grid rendering with Cook-Torrance PBR.
//
// Renders a 300×300 vertex grid displaced along Y by the height map.
// PBR shading with metallic=1.0, roughness=0.05, and procedural IBL
// for the chrome-like glass reflections.
//
// Grid is procedurally generated from vertex_index:
//   299×299 quads × 6 vertices = 536,406 vertices total.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_pos: vec4<f32>,
    light_color: vec4<f32>,     // rgb = color, a = intensity
    material: vec4<f32>,        // x = metallic, y = roughness, z = displacement, w = unused
    grid_info: vec4<f32>,       // x = grid_size (300), y = texel_size (1/width), z = unused, w = unused
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var height_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

// ─── Vertex Shader ─────────────────────────────────────────────────

fn sample_height(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(height_tex, tex_sampler, uv, 0.0).r;
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let grid_size = u32(u.grid_info.x);   // 300
    let quads = grid_size - 1u;            // 299

    // Decode vertex_index → quad + corner
    let quad_idx = vertex_index / 6u;
    let corner = vertex_index % 6u;
    let quad_x = quad_idx % quads;
    let quad_y = quad_idx / quads;

    // Corner offsets: triangle 1 (0,0)(1,0)(0,1), triangle 2 (0,1)(1,0)(1,1)
    var dx: u32;
    var dy: u32;
    switch corner {
        case 0u: { dx = 0u; dy = 0u; }
        case 1u: { dx = 1u; dy = 0u; }
        case 2u: { dx = 0u; dy = 1u; }
        case 3u: { dx = 0u; dy = 1u; }
        case 4u: { dx = 1u; dy = 0u; }
        case 5u: { dx = 1u; dy = 1u; }
        default: { dx = 0u; dy = 0u; }
    }

    let vx = quad_x + dx;
    let vy = quad_y + dy;
    let uv = vec2<f32>(f32(vx) / f32(quads), f32(vy) / f32(quads));

    // Grid spans [-1, 1] on XZ plane
    let world_x = uv.x * 2.0 - 1.0;
    let world_z = uv.y * 2.0 - 1.0;

    // Displacement along Y from height map
    let displacement = u.material.z;
    let h = sample_height(uv) * displacement;

    let world_pos = vec3<f32>(world_x, h, world_z);

    // Compute normal via finite differences on the height map
    let eps = 1.0 / f32(quads);  // one grid cell
    let h_px = sample_height(uv + vec2(eps, 0.0)) * displacement;
    let h_nx = sample_height(uv - vec2(eps, 0.0)) * displacement;
    let h_py = sample_height(uv + vec2(0.0, eps)) * displacement;
    let h_ny = sample_height(uv - vec2(0.0, eps)) * displacement;

    // Tangent vectors in world space (grid spans 2 units, so dx = 2*eps)
    let dx_world = 2.0 * eps;
    let tangent_x = vec3<f32>(dx_world, h_px - h_nx, 0.0);
    let tangent_z = vec3<f32>(0.0, h_py - h_ny, dx_world);
    let normal = normalize(cross(tangent_z, tangent_x));

    var out: VertexOutput;
    out.clip_pos = u.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.world_normal = normal;
    out.uv = uv;
    return out;
}

// ─── PBR: Cook-Torrance BRDF ───────────────────────────────────────

const PI: f32 = 3.14159265358979;

// GGX Normal Distribution Function
fn D_GGX(NdotH: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let denom = NdotH * NdotH * (a2 - 1.0) + 1.0;
    return a2 / (PI * denom * denom);
}

// Smith's Geometry Function (Schlick-GGX)
fn G_SchlickGGX(NdotV: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return NdotV / (NdotV * (1.0 - k) + k);
}

fn G_Smith(NdotV: f32, NdotL: f32, roughness: f32) -> f32 {
    return G_SchlickGGX(NdotV, roughness) * G_SchlickGGX(NdotL, roughness);
}

// Schlick Fresnel
fn F_Schlick(cosTheta: f32, F0: vec3<f32>) -> vec3<f32> {
    return F0 + (1.0 - F0) * pow(1.0 - cosTheta, 5.0);
}

// ─── Procedural Environment ────────────────────────────────────────
// Studio-like environment for metallic reflections.
// Bright band near the horizon creates the characteristic chrome look.

fn env_color(dir: vec3<f32>) -> vec3<f32> {
    let up = dir.y;

    // Bright band near horizon (simulates studio windows/lights)
    let horizon = exp(-12.0 * up * up) * 2.5;

    // Secondary highlight band above (simulates overhead soft box)
    let overhead = smoothstep(0.3, 0.6, up) * smoothstep(0.9, 0.6, up) * 1.8;

    // Ground bounce (subtle)
    let ground = max(-up, 0.0) * 0.15;

    // Sky gradient
    let sky = max(up, 0.0) * 0.2;

    let intensity = horizon + overhead + ground + sky + 0.05;

    // Slight warm/cool color variation
    return vec3<f32>(
        intensity * 0.97,
        intensity * 1.0,
        intensity * 1.04,
    );
}

// ─── Fragment Shader ───────────────────────────────────────────────

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let N = normalize(in.world_normal);
    let V = normalize(u.camera_pos.xyz - in.world_pos);

    let metallic = u.material.x;
    let roughness = max(u.material.y, 0.01);

    // Base color for metallic surface (silver/chrome)
    let base_color = vec3<f32>(0.8, 0.8, 0.85);

    // F0: for metals, F0 = base_color
    let F0 = mix(vec3(0.04), base_color, metallic);

    let NdotV = max(dot(N, V), 0.001);

    // ── Direct lighting ──
    let L = normalize(u.light_pos.xyz - in.world_pos);
    let H = normalize(V + L);
    let NdotL = max(dot(N, L), 0.0);
    let NdotH = max(dot(N, H), 0.0);
    let VdotH = max(dot(V, H), 0.001);

    let D = D_GGX(NdotH, roughness);
    let G = G_Smith(NdotV, NdotL, roughness);
    let F = F_Schlick(VdotH, F0);

    let numerator = D * G * F;
    let denominator = 4.0 * NdotV * NdotL + 0.0001;
    let specular = numerator / denominator;

    // For metals, there is no diffuse component (all energy is specular)
    let kD = (1.0 - F) * (1.0 - metallic);
    let diffuse = kD * base_color / PI;

    let light_intensity = u.light_color.a;
    let direct = (diffuse + specular) * u.light_color.rgb * light_intensity * NdotL;

    // ── Image-Based Lighting (procedural environment) ──
    let R = reflect(-V, N);
    let env = env_color(R);

    // Fresnel for environment reflection
    let F_env = F_Schlick(NdotV, F0);

    // Approximate environment BRDF integration
    // For low roughness, environment reflection is dominant
    let env_roughness_scale = 1.0 - roughness * 0.7;
    let ibl = env * F_env * env_roughness_scale;

    // ── Combine ──
    let color = direct + ibl;

    // Simple Reinhard tonemap
    let mapped = color / (color + vec3(1.0));

    return vec4<f32>(mapped, 1.0);
}
