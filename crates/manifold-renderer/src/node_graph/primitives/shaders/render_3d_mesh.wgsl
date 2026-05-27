// node.render_3d_mesh — vertex+fragment pipeline that draws an
// Array<MeshVertex> as a triangle list with depth testing and a
// per-MaterialKind fragment shader (Unlit / Phong / PBR / Cel).
//
// Material system M4: the renderer holds an AHashMap<MaterialKind,
// GpuRenderPipeline> and dispatches the matching pipeline per the
// wired `material: Material` input. The uniform is the SUPERSET of
// every kind's params; fragments only read what their kind needs.
// Naga rule: multi-entry-point WGSL must declare the same uniform
// shape at the same binding — we therefore share one Uniforms struct
// across every entry point in this file. Per-entry-point MSL is
// emitted by naga with only the bindings each entry actually
// accesses, so unlit/phong/cel pipelines don't reference the envmap
// binding even though the WGSL declares it.
//
// MeshVertex layout (48 bytes):
//   position: vec3<f32> + pad
//   normal:   vec3<f32> + pad
//   uv:       vec2<f32> + pad
//
// Topology: every 3 consecutive vertices form one triangle.
// The vertex shader looks up vertex `vertex_index` directly from the
// storage buffer — no vertex buffer binding.
//
// Surface textures sample at the per-vertex `uv` channel interpolated
// through the rasterizer. This is the industry-standard mesh-UV
// pattern (Blender, Unreal, Unity, TouchDesigner) — the texel a
// fragment reads depends on where the fragment lies on the parametric
// surface, not where it lands on screen. Texture detail follows the
// geometry as the camera orbits.
//
// Entry points:
//   fs_unlit         — flat colour passthrough + emission
//   fs_phong         — Lambert diffuse + Blinn-Phong specular
//   fs_pbr           — Cook-Torrance D_GGX * G_Smith * F_Schlick + IBL
//   fs_cel           — Lambert N·L quantized into cel_bands
//   fs_world_pos     — emit interpolated world position (G-buffer)
//   fs_world_normal  — emit normalised world-space surface normal (G-buffer)

const PI: f32 = 3.14159265358979;

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

// Superset uniform — fields inert for kinds that don't read them.
// 16-byte aligned. Total: 64 + 16*9 = 208 bytes.
struct Uniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // Direct light direction (xyz: from-surface-toward-light unit
    // vector, w: intensity multiplier). Light colour is premultiplied
    // with intensity by the producer; w stays available as an extra
    // tweak knob for unwired-light scenarios (kept = 1.0 currently).
    light_dir: vec4<f32>,
    // rgb: light colour PREMULTIPLIED with intensity, w: ambient.
    light_color: vec4<f32>,
    // rgb: surface diffuse / base colour, w: opacity (informational
    // for v1; opaque-only rendering).
    base_color: vec4<f32>,
    // rgb: emission PREMULTIPLIED with intensity, w: reserved (1.0).
    emission: vec4<f32>,
    // x: metallic [0,1], y: roughness [0.01,1], z/w: reserved.
    pbr_metallic_roughness: vec4<f32>,
    // rgb: specular tint, w: Phong exponent.
    specular: vec4<f32>,
    // x: cel_bands (count, as f32), y: band_low, z: band_high, w: reserved.
    cel_params: vec4<f32>,
    // x: use_normal_map (0/1), y: use_roughness_map (0/1), z/w: reserved.
    // 1.0 = sample the corresponding texture at in.uv; 0.0 = fall back
    // to the scalar value baked into the material (or the geometry's
    // own per-vertex normal for the normal channel).
    texture_flags: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
// PBR-only IBL envmap. Equirectangular HDR. Bound only by the PBR
// pipeline at draw time; other pipelines never sample it.
@group(0) @binding(2) var envmap: texture_2d<f32>;
@group(0) @binding(3) var envmap_sampler: sampler;
// Surface textures sampled at the per-vertex mesh UV. Both are
// optional inputs on the renderer; when unwired the renderer binds
// a 1×1 dummy texture and `texture_flags` gates the sampling so the
// material's scalar values take over.
//
// `normal_map` is a WORLD-SPACE signed normal (matches the convention
// produced by `node.heightmap_to_normal` in WorldYUp mode). Tangent-
// space normal maps are a future extension that requires per-vertex
// tangents.
//
// `roughness_map`'s red channel replaces `pbr_metallic_roughness.y`
// when wired.
@group(0) @binding(4) var normal_map: texture_2d<f32>;
@group(0) @binding(5) var roughness_map: texture_2d<f32>;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let v = verts[vid];
    var out: VsOut;
    out.clip_pos = u.view_proj * vec4<f32>(v.position, 1.0);
    out.world_pos = v.position;
    out.world_normal = v.normal;
    out.uv = v.uv;
    return out;
}

// Resolve the surface normal per-fragment: when `normal_map` is wired
// (texture_flags.x ≈ 1.0), sample at mesh UV and renormalise; otherwise
// use the rasterizer-interpolated vertex normal. World-space convention
// — matches `node.heightmap_to_normal`'s WorldYUp output.
fn resolve_normal(uv: vec2<f32>, vertex_normal: vec3<f32>) -> vec3<f32> {
    if u.texture_flags.x > 0.5 {
        let sampled = textureSampleLevel(normal_map, envmap_sampler, uv, 0.0).rgb;
        let n = sampled + vec3<f32>(1e-8, 0.0, 0.0);
        return normalize(n);
    }
    return normalize(vertex_normal);
}

// Resolve roughness per-fragment: roughness_map.r at mesh UV when
// wired, else the material's scalar. Clamped to [0.01, 1.0] (D_GGX
// blows up at 0).
fn resolve_roughness(uv: vec2<f32>) -> f32 {
    var r: f32;
    if u.texture_flags.y > 0.5 {
        r = textureSampleLevel(roughness_map, envmap_sampler, uv, 0.0).r;
    } else {
        r = u.pbr_metallic_roughness.y;
    }
    return max(r, 0.01);
}

// ===== Material kind fragment entry points =====

// Unlit — flat colour passthrough plus emission. No lighting math.
@fragment
fn fs_unlit(in: VsOut) -> @location(0) vec4<f32> {
    let rgb = u.base_color.rgb + u.emission.rgb;
    return vec4<f32>(rgb, u.base_color.a);
}

// Phong — Lambert diffuse + Blinn-Phong specular.
@fragment
fn fs_phong(in: VsOut) -> @location(0) vec4<f32> {
    let N = resolve_normal(in.uv, in.world_normal);
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

// PBR — Cook-Torrance microfacet specular + Lambert diffuse + IBL
// reflection from envmap. F0 blends 0.04 dielectric ↔ base_color metal.
@fragment
fn fs_pbr(in: VsOut) -> @location(0) vec4<f32> {
    let N = resolve_normal(in.uv, in.world_normal);
    let L = normalize(u.light_dir.xyz);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    let H = normalize(L + V);
    let metallic = clamp(u.pbr_metallic_roughness.x, 0.0, 1.0);
    let roughness = resolve_roughness(in.uv);

    let n_dot_l = max(dot(N, L), 0.0);
    let n_dot_v = max(dot(N, V), 0.001);
    let n_dot_h = max(dot(N, H), 0.0);
    let v_dot_h = max(dot(V, H), 0.001);

    // F0 — dielectric 0.04 lerped to base_color for metals.
    let F0 = mix(vec3<f32>(0.04), u.base_color.rgb, metallic);

    // GGX D term.
    let a = roughness * roughness;
    let a2 = a * a;
    let denom_d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    let D = a2 / (PI * denom_d * denom_d);

    // Smith G term (Schlick-GGX paired).
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let g_v = n_dot_v / (n_dot_v * (1.0 - k) + k);
    let g_l = n_dot_l / (n_dot_l * (1.0 - k) + k);
    let G = g_v * g_l;

    // Schlick Fresnel.
    let F = F0 + (1.0 - F0) * pow(clamp(1.0 - v_dot_h, 0.0, 1.0), 5.0);

    let specular = (D * G * F) / (4.0 * n_dot_v * n_dot_l + 0.0001);

    // Lambert diffuse (dielectric only — metals have no diffuse term).
    let kd = (1.0 - F) * (1.0 - metallic);
    let diffuse = kd * u.base_color.rgb / PI;

    let direct = (diffuse + specular) * u.light_color.rgb * n_dot_l * u.light_dir.w;

    // IBL reflection — sample envmap along reflected view direction.
    let R = reflect(-V, N);
    let azimuth = atan2(R.z, R.x);
    let elevation = asin(clamp(R.y, -1.0, 1.0));
    let uv = vec2<f32>(azimuth / (2.0 * PI) + 0.5, elevation / PI + 0.5);
    // Roughness-driven attenuation. envmap is a single mip in v1.
    let ibl_sample = textureSampleLevel(envmap, envmap_sampler, uv, 0.0).rgb;
    let ibl_strength = 1.0 - roughness * 0.7;
    let ibl = F * ibl_sample * ibl_strength;

    let ambient = u.base_color.rgb * u.light_color.a;
    let rgb = direct + ibl + ambient + u.emission.rgb;
    return vec4<f32>(rgb, u.base_color.a);
}

// Cel — Lambert N·L quantized into cel_bands discrete steps between
// band_low (shadow) and band_high (lit).
@fragment
fn fs_cel(in: VsOut) -> @location(0) vec4<f32> {
    let N = resolve_normal(in.uv, in.world_normal);
    let L = normalize(u.light_dir.xyz);
    let n_dot_l = max(dot(N, L), 0.0);
    let bands = max(u.cel_params.x, 2.0);
    let band_low = u.cel_params.y;
    let band_high = u.cel_params.z;
    // Snap n_dot_l to one of `bands` discrete levels in [0, 1].
    let snapped = floor(n_dot_l * bands) / (bands - 1.0);
    let level = mix(band_low, band_high, clamp(snapped, 0.0, 1.0));
    let lit = u.base_color.rgb * level * u.light_color.rgb * u.light_dir.w;
    return vec4<f32>(lit + u.emission.rgb, u.base_color.a);
}

// ===== G-buffer outputs (preserved from pre-Material design) =====

// Emit interpolated world position (XYZ + alpha=1 for geometry coverage).
@fragment
fn fs_world_pos(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.world_pos, 1.0);
}

// Emit normalised world-space surface normal in [-1, 1] (alpha=1).
@fragment
fn fs_world_normal(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    return vec4<f32>(n, 1.0);
}
