// Metallic Glass — Pass 5: Displaced grid + PBR rendering.
//
// Replicates TD rendering chain:
//   Grid SOP: 300×300 vertices
//   Displacement: Height Map × weight along Y
//   PBR MAT: Metallic 1.0, Roughness 0.05
//   Point Light: Intensity 3.5, Position (-2, 2, 5)
//   Environment Light: HDR studio (procedural approximation)
//   Camera: 35mm focal length (~54° FOV), looking slightly up at grid
//
// Normals are computed per-pixel in the fragment shader (not per-vertex)
// for full-resolution reflections on the 300×300 grid.

struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    light_pos: vec4<f32>,       // TD: X=-2, Y=2, Z=5
    light_color: vec4<f32>,     // rgb = color, a = intensity (TD: 3.5)
    material: vec4<f32>,        // x = metallic (1.0), y = roughness (0.05), z = displacement (0.2), w = unused
    grid_info: vec4<f32>,       // x = grid_size (300), y = texel_size (1/tex_width)
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var height_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

// ─── Vertex Shader ─────────────────────────────────────────────────

fn sample_height(uv: vec2<f32>) -> f32 {
    return textureSampleLevel(height_tex, tex_sampler, uv, 0.0).r;
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let grid_size = u32(u.grid_info.x);
    let quads = grid_size - 1u;

    let quad_idx = vertex_index / 6u;
    let corner = vertex_index % 6u;
    let quad_x = quad_idx % quads;
    let quad_y = quad_idx / quads;

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

    let world_x = uv.x * 2.0 - 1.0;
    let world_z = uv.y * 2.0 - 1.0;

    let displacement = u.material.z;
    let h = sample_height(uv) * displacement;
    let world_pos = vec3<f32>(world_x, h, world_z);

    var out: VertexOutput;
    out.clip_pos = u.view_proj * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.uv = uv;
    return out;
}

// ─── PBR: Cook-Torrance BRDF ───────────────────────────────────────

const PI: f32 = 3.14159265358979;

fn D_GGX(NdotH: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let denom = NdotH * NdotH * (a2 - 1.0) + 1.0;
    return a2 / (PI * denom * denom);
}

fn G_SchlickGGX(NdotV: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return NdotV / (NdotV * (1.0 - k) + k);
}

fn G_Smith(NdotV: f32, NdotL: f32, roughness: f32) -> f32 {
    return G_SchlickGGX(NdotV, roughness) * G_SchlickGGX(NdotL, roughness);
}

fn F_Schlick(cosTheta: f32, F0: vec3<f32>) -> vec3<f32> {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

// ─── Procedural HDR Environment ────────────────────────────────────
// Approximates a high-contrast studio interior HDR.
// Raised ambient floor (0.15) so no direction reflects pure black —
// real studio HDRs have bounced light everywhere.

fn env_color(dir: vec3<f32>) -> vec3<f32> {
    let up = dir.y;
    let azimuth = atan2(dir.z, dir.x);

    // Studio ambient floor — no direction should be pure black
    var color = vec3<f32>(0.15, 0.15, 0.17);

    // Large bright horizon band (studio windows / white cyclorama)
    color += vec3(1.5, 1.45, 1.4) * exp(-15.0 * up * up);

    // Overhead soft box
    let overhead = smoothstep(0.35, 0.65, up) * smoothstep(0.95, 0.65, up);
    color += vec3(2.5, 2.4, 2.3) * overhead;

    // Floor fill (bounced light from below)
    let floor_fill = smoothstep(-0.15, -0.45, up) * smoothstep(-0.85, -0.45, up);
    color += vec3(0.4, 0.42, 0.45) * floor_fill;

    // Two narrow strip lights (create chrome streaks)
    color += vec3(3.5, 3.2, 2.8) * exp(-300.0 * pow(up - 0.12, 2.0));
    color += vec3(1.5, 2.0, 3.0) * exp(-300.0 * pow(up + 0.08, 2.0));

    // Azimuthal variation
    color *= sin(azimuth * 2.0) * 0.12 + 1.0;

    return color;
}

// ─── Fragment Shader ───────────────────────────────────────────────
// Per-pixel normal computation from height map for full-resolution
// reflections (not limited by 300×300 grid vertex density).

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let V = normalize(u.camera_pos.xyz - in.world_pos);

    let metallic = u.material.x;
    let roughness = max(u.material.y, 0.01);
    let displacement = u.material.z;

    // Per-pixel normal from height map finite differences.
    // Uses texture texel size for full-resolution normal detail.
    let texel = u.grid_info.y;  // 1.0 / texture_width
    let h_px = textureSampleLevel(height_tex, tex_sampler, in.uv + vec2(texel, 0.0), 0.0).r;
    let h_nx = textureSampleLevel(height_tex, tex_sampler, in.uv - vec2(texel, 0.0), 0.0).r;
    let h_py = textureSampleLevel(height_tex, tex_sampler, in.uv + vec2(0.0, texel), 0.0).r;
    let h_ny = textureSampleLevel(height_tex, tex_sampler, in.uv - vec2(0.0, texel), 0.0).r;

    // World-space tangent vectors (grid spans 2 units, texel = 1/tex_width)
    let dx_world = 2.0 * texel;
    let dh_x = (h_px - h_nx) * displacement;
    let dh_z = (h_py - h_ny) * displacement;
    let tangent_x = vec3<f32>(dx_world, dh_x, 0.0);
    let tangent_z = vec3<f32>(0.0, dh_z, dx_world);
    let N = normalize(cross(tangent_z, tangent_x));

    // Base color: neutral silver (metallic F0 = base_color)
    let base_color = vec3<f32>(0.8, 0.8, 0.82);
    let F0 = mix(vec3(0.04), base_color, metallic);

    let NdotV = max(dot(N, V), 0.001);

    // ── Direct lighting (TD Point Light: pos (-2,2,5), intensity 3.5) ──
    let L = normalize(u.light_pos.xyz - in.world_pos);
    let H = normalize(V + L);
    let NdotL = max(dot(N, L), 0.0);
    let NdotH = max(dot(N, H), 0.0);
    let VdotH = max(dot(V, H), 0.001);

    let light_dist = length(u.light_pos.xyz - in.world_pos);
    let attenuation = 1.0 / (1.0 + light_dist * light_dist / 25.0);

    let D = D_GGX(NdotH, roughness);
    let G = G_Smith(NdotV, NdotL, roughness);
    let F = F_Schlick(VdotH, F0);

    let spec = (D * G * F) / (4.0 * NdotV * NdotL + 0.0001);
    let kD = (1.0 - F) * (1.0 - metallic);
    let diffuse = kD * base_color / PI;

    let light_intensity = u.light_color.a;
    let direct = (diffuse + spec) * u.light_color.rgb * light_intensity * NdotL * attenuation;

    // ── Environment IBL (TD Environment Light COMP) ──
    let R = reflect(-V, N);
    let env = env_color(R);
    let F_env = F_Schlick(NdotV, F0);
    let env_scale = 1.0 - roughness * 0.7;
    let ibl = env * F_env * env_scale;

    // ── Combine ──
    let color = direct + ibl;
    let mapped = color / (color + vec3(1.0));

    return vec4<f32>(mapped, 1.0);
}
