// node.render_scene — multi-object 3D scene renderer: N objects sharing
// ONE depth buffer (real occlusion) lit by up to 4 lights. Forked from
// render_3d_mesh.wgsl (the single-object renderer): keeps every
// Material-system M4/M6 addition (resolve_albedo / resolve_metallic,
// alpha-cutout discard, view-facing normal flip) byte-identical, and
// adds exactly two things render_3d_mesh doesn't need:
//
//   1. A per-object `model` matrix, composed CPU-side from that
//      object's pos/rot/scale params (see
//      node_graph/primitives/render_scene.rs::model_matrix). One draw
//      call per object, same shared depth texture — the CPU side draws
//      object 0 with GpuLoadAction::Clear and objects 1..N with
//      GpuLoadAction::Load, so the depth test resolves real occlusion
//      between objects instead of each rendering into its own buffer.
//   2. A `lights: array<vec4<f32>, 8>` accumulator (up to 4 lights, 2
//      vec4s each — `lights[i*2]` = dir.xyz + intensity in .w,
//      `lights[i*2+1]` = premultiplied color.rgb) so the Phong/PBR/Cel
//      entry points sum every wired light's direct term instead of
//      reading exactly one `light_dir`/`light_color` pair. Ambient and
//      emission are added exactly once (after the light loop), not
//      blended per light.
//
// Shadows (P2) via the caster table + PCF below; no atmosphere/fog yet
// (P3). No per-object surface
// textures in P1 — `texture_flags` is always zero (render_scene has no
// normal_map / roughness_map / base_color_map / metallic_map inputs);
// the resolve_* helpers below are kept identical to render_3d_mesh.wgsl
// so a future per-object texture extension is a pure additive port add,
// not a shader rewrite. ONE envmap is shared across every PBR object in
// the scene (an environment map is scene-wide, not per-object).
//
// MeshVertex layout (48 bytes), entry point names, and per-kind
// dispatch: identical to render_3d_mesh.wgsl.

const PI: f32 = 3.14159265358979;

struct Vertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    _pad1: f32,
    uv: vec2<f32>,
    _pad2: vec2<f32>,
};

// Superset uniform, rebuilt once per object per draw call. 16-byte
// aligned throughout (two mat4x4s + nine vec4s — every member is already
// a vec4/mat4 multiple, so no manual padding is needed). Total 272 bytes.
// Lights are NO LONGER in here: they live in the `@binding(8)` storage
// buffer below (a scene can carry any number of lights, not the old 4).
struct Uniforms {
    view_proj: mat4x4<f32>,
    // This object's world transform, composed CPU-side from
    // pos_x/y/z + rot_x/y/z (Euler, X→Y→Z order, matching
    // render_instanced_3d_mesh.wgsl's euler_xyz) + scale_x/y/z.
    // v1 ignores non-uniform-scale normal skew (no inverse-transpose
    // applied to the normal) — fine for uniform/near-uniform scale, a
    // known limitation for extreme non-uniform scale.
    model: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // rgb: surface diffuse / base colour, w: opacity (informational).
    base_color: vec4<f32>,
    // rgb: emission PREMULTIPLIED with intensity, w: reserved.
    emission: vec4<f32>,
    // x: metallic [0,1], y: roughness [0.01,1], z/w: reserved.
    pbr_metallic_roughness: vec4<f32>,
    // rgb: specular tint, w: Phong exponent.
    specular: vec4<f32>,
    // x: cel_bands (as f32), y: band_low, z: band_high, w: reserved.
    cel_params: vec4<f32>,
    // Surface-texture presence flags — always 0 in render_scene (no
    // per-object surface texture inputs in P1). Kept so resolve_* below
    // stays byte-identical to render_3d_mesh.wgsl.
    texture_flags: vec4<f32>,
    // x: alpha_mode (1.0 = Mask/cutout, 0.0 = Opaque), y: alpha_cutoff,
    // z/w: reserved.
    alpha_params: vec4<f32>,
    // x: light_count (as f32, unbounded — the `lights` storage buffer is
    // runtime-sized), y: ambient (this object's material.ambient),
    // z/w: reserved.
    scene_params: vec4<f32>,
    // Atmosphere (P3), scene-wide (same in every object's uniform).
    // fog_color.rgb = colour distant geometry fades toward.
    fog_color: vec4<f32>,
    // x: fog_density (0 = no fog), y: height_falloff (0 = uniform),
    // z/w: reserved.
    fog_params: vec4<f32>,
    // rgb: ambient/sky tint multiplier on the ambient term (1,1,1 = neutral).
    ambient_tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
// PBR-only IBL envmap — ONE shared equirect map for every PBR object in
// the scene.
@group(0) @binding(2) var envmap: texture_2d<f32>;
@group(0) @binding(3) var envmap_sampler: sampler;
// No per-object surface textures in P1 — these bind the 1×1 dummy every
// draw; texture_flags (always 0) keeps resolve_* from ever sampling them.
@group(0) @binding(4) var normal_map: texture_2d<f32>;
@group(0) @binding(5) var roughness_map: texture_2d<f32>;
@group(0) @binding(6) var base_color_map: texture_2d<f32>;
@group(0) @binding(7) var metallic_map: texture_2d<f32>;
// Scene lights, 2 vec4s each, runtime-sized (was a fixed `array<vec4,8>`
// inside Uniforms; the old cap of 4 lights is gone). Only the first
// `scene_params.x` entries are meaningful — count flows through the
// uniform, NOT `arrayLength` (D1). At zero lights the CPU still binds one
// zeroed entry so Metal always sees a bound buffer (D4).
//   lights[i*2]   = (dir.xyz: from-surface-toward-light, w: intensity)
//   lights[i*2+1] = (color.rgb PREMULTIPLIED with intensity, w: unused)
@group(0) @binding(8) var<storage, read> lights: array<vec4<f32>>;

// Shadow-caster table: MAX_SHADOW_CASTING_LIGHTS (=4) slots × 5 vec4.
// Per slot: [0..3] = the caster's light-space view_proj columns,
// [4] = (bias, kernel_half_width, texel_size, 0). Filled only for active
// casters (zeroed otherwise); a light's caster slot rides lights[i*2+1].w
// (−1.0 = this light casts no shadow). The four shadow maps are separate
// bindings because WGSL has no dynamic texture-binding indexing — the K=4
// switch in sample_shadow() picks the right one.
@group(0) @binding(9) var<storage, read> casters: array<vec4<f32>>;
@group(0) @binding(10) var shadow_map_0: texture_depth_2d;
@group(0) @binding(11) var shadow_map_1: texture_depth_2d;
@group(0) @binding(12) var shadow_map_2: texture_depth_2d;
@group(0) @binding(13) var shadow_map_3: texture_depth_2d;
@group(0) @binding(14) var shadow_sampler: sampler_comparison;

// Per-object instancing (REALTIME_3D_DESIGN.md §10 D11+P8). One always-bound
// storage buffer per object: wired instances_n binds the real buffer,
// unwired binds a cached 1-entry identity stub (pos_scale [0,0,0,1],
// rot_pad zeros) drawn with instance_count 1 — the same always-bind
// ABI-stub pattern as the shadow bindings above. Layout matches
// generators::mesh_common::InstanceTransform (32 bytes) and
// render_instanced_3d_mesh.wgsl's `Instance` exactly.
struct Instance {
    pos_scale: vec4<f32>,
    rot_pad: vec4<f32>,
};
@group(0) @binding(15) var<storage, read> instances: array<Instance>;

// Build a 3×3 rotation matrix from XYZ Euler angles (XYZ order).
// Bit-for-bit the same as render_instanced_3d_mesh.wgsl's euler_xyz —
// forked, not shared, per this file's header convention.
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

const CASTER_STRIDE: u32 = 5u;

// Pick shadow map `slot` (0..3) and do one hardware comparison sample.
// `textureSampleCompareLevel` (level 0, no derivatives) is legal in the
// data-dependent control flow the per-light loop creates — plain
// `textureSampleCompare` would not be.
fn sample_shadow(slot: i32, suv: vec2<f32>, ref_depth: f32) -> f32 {
    switch slot {
        case 0: { return textureSampleCompareLevel(shadow_map_0, shadow_sampler, suv, ref_depth); }
        case 1: { return textureSampleCompareLevel(shadow_map_1, shadow_sampler, suv, ref_depth); }
        case 2: { return textureSampleCompareLevel(shadow_map_2, shadow_sampler, suv, ref_depth); }
        default: { return textureSampleCompareLevel(shadow_map_3, shadow_sampler, suv, ref_depth); }
    }
}

// Light visibility in [0,1]: 1 = fully lit, 0 = fully shadowed. `slot_f` is
// lights[i*2+1].w — negative means this light casts no shadow, so the point
// is always lit. Reconstructs the fragment's light-space position, does a
// PCF depth compare with a (2·khw+1)² kernel. Points outside the caster's
// frustum read as lit (no shadow data there) rather than clamped-dark.
fn shadow_factor(world_pos: vec3<f32>, slot_f: f32) -> f32 {
    if slot_f < 0.0 {
        return 1.0;
    }
    let slot = i32(slot_f + 0.5);
    let base = u32(slot) * CASTER_STRIDE;
    let vp = mat4x4<f32>(
        casters[base],
        casters[base + 1u],
        casters[base + 2u],
        casters[base + 3u],
    );
    let params = casters[base + 4u];
    let bias = params.x;
    let khw = i32(params.y);
    let texel = params.z;

    let clip = vp * vec4<f32>(world_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    // NDC xy [-1,1] → uv [0,1], y flipped (Metal texture origin is top-left,
    // NDC y points up). Metal clip depth is already [0,1] in z.
    let suv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    if suv.x < 0.0 || suv.x > 1.0 || suv.y < 0.0 || suv.y > 1.0 || ndc.z > 1.0 || ndc.z < 0.0 {
        return 1.0;
    }
    let ref_depth = ndc.z - bias;

    var sum = 0.0;
    var count = 0.0;
    for (var dy = -khw; dy <= khw; dy = dy + 1) {
        for (var dx = -khw; dx <= khw; dx = dx + 1) {
            let off = vec2<f32>(f32(dx), f32(dy)) * texel;
            sum = sum + sample_shadow(slot, suv + off, ref_depth);
            count = count + 1.0;
        }
    }
    return sum / count;
}

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
};

// Instance TRS applies FIRST, the object group's `model` (transform_n)
// SECOND — world = model_n · T_instance (D11). An identity instance
// (unwired instances_n) collapses `rot` to the identity matrix and
// `inst_pos`/`inst_normal` to `v.position`/`v.normal` unchanged, so this
// is byte-identical to the pre-P8 vertex shader for every unwired object.
@vertex
fn vs_main(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) iid: u32,
) -> VsOut {
    let v = verts[vid];
    let inst = instances[iid];
    let rot = euler_xyz(inst.rot_pad.xyz);
    let inst_pos = rot * (v.position * inst.pos_scale.w) + inst.pos_scale.xyz;
    let inst_normal = rot * v.normal;

    var out: VsOut;
    let world = u.model * vec4<f32>(inst_pos, 1.0);
    out.world_pos = world.xyz;
    out.clip_pos = u.view_proj * world;
    out.world_normal = normalize((u.model * vec4<f32>(inst_normal, 0.0)).xyz);
    out.uv = v.uv;
    return out;
}

// Identical to render_3d_mesh.wgsl — see that file for fuller commentary.
fn resolve_normal(uv: vec2<f32>, vertex_normal: vec3<f32>) -> vec3<f32> {
    if u.texture_flags.x > 0.5 {
        let sampled = textureSampleLevel(normal_map, envmap_sampler, uv, 0.0).rgb;
        let n = sampled + vec3<f32>(1e-8, 0.0, 0.0);
        return normalize(n);
    }
    return normalize(vertex_normal);
}

fn resolve_roughness(uv: vec2<f32>) -> f32 {
    var r: f32;
    if u.texture_flags.y > 0.5 {
        r = textureSampleLevel(roughness_map, envmap_sampler, uv, 0.0).r;
    } else {
        r = u.pbr_metallic_roughness.y;
    }
    return max(r, 0.01);
}

fn resolve_albedo(uv: vec2<f32>) -> vec4<f32> {
    if u.texture_flags.z > 0.5 {
        let t = textureSampleLevel(base_color_map, envmap_sampler, uv, 0.0);
        return vec4<f32>(u.base_color.rgb * t.rgb, u.base_color.a * t.a);
    }
    return u.base_color;
}

fn resolve_metallic(uv: vec2<f32>) -> f32 {
    if u.texture_flags.w > 0.5 {
        return textureSampleLevel(metallic_map, envmap_sampler, uv, 0.0).r;
    }
    return u.pbr_metallic_roughness.x;
}

// Exponential depth fog (P3), applied to a lit fragment's STRAIGHT
// (non-premultiplied) rgb just before return. Distance is camera→fragment;
// height_falloff scales density by exp(-falloff·max(y,0)) so fog thins with
// altitude (ground haze). Alpha is left untouched — fog composits OVER the
// premultiplied-alpha contract, it does not replace it, so a transparent
// fragment stays transparent and keys downstream. fog_density 0 → factor 0
// → identity, so an unwired atmosphere is byte-identical to no atmosphere.
fn apply_fog(rgb: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    let density = u.fog_params.x;
    if density <= 0.0 {
        return rgb;
    }
    let falloff = u.fog_params.y;
    let dist = length(u.camera_pos.xyz - world_pos);
    let h = exp(-falloff * max(world_pos.y, 0.0));
    let fog = clamp(1.0 - exp(-density * dist * h), 0.0, 1.0);
    return mix(rgb, u.fog_color.rgb, fog);
}

// ===== Material kind fragment entry points =====

// Unlit — flat colour passthrough plus emission. No lighting math, no
// light loop (matches render_3d_mesh.wgsl exactly).
@fragment
fn fs_unlit(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = resolve_albedo(in.uv);
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    let rgb = apply_fog(albedo.rgb + u.emission.rgb, in.world_pos);
    return vec4<f32>(rgb, albedo.a);
}

// Phong — Lambert diffuse + Blinn-Phong specular, summed over every
// wired light; ambient + emission added exactly once (not blended per
// light the way the single-light render_3d_mesh.wgsl does it).
@fragment
fn fs_phong(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = resolve_albedo(in.uv);
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    var N = resolve_normal(in.uv, in.world_normal);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    if dot(N, V) < 0.0 {
        N = -N;
    }

    var lit = vec3<f32>(0.0);
    let light_count = u32(u.scene_params.x);
    for (var i = 0u; i < light_count; i = i + 1u) {
        let l_dir = lights[i * 2u];
        let l_col = lights[i * 2u + 1u];
        let L = normalize(l_dir.xyz);
        let H = normalize(L + V);
        let n_dot_l = max(dot(N, L), 0.0);
        let n_dot_h = max(dot(N, H), 0.0);
        let diffuse = albedo.rgb * n_dot_l;
        let spec = u.specular.rgb * pow(n_dot_h, max(u.specular.w, 1.0)) * n_dot_l;
        let vis = shadow_factor(in.world_pos, l_col.w);
        lit = lit + (diffuse + spec) * l_col.rgb * l_dir.w * vis;
    }
    let ambient = albedo.rgb * u.scene_params.y * u.ambient_tint.rgb;
    let rgb = apply_fog(lit + ambient + u.emission.rgb, in.world_pos);
    return vec4<f32>(rgb, albedo.a);
}

// PBR — Cook-Torrance microfacet specular + Lambert diffuse, summed over
// every wired light (H/D/G/F/kd/diffuse are all light-dependent via H,
// so they're recomputed per light); IBL reflection + ambient + emission
// added exactly once outside the loop. IBL's Fresnel term uses N·V
// (view-only Schlick) rather than the light-dependent N·H term any
// single light would give — the standard split-sum substitute for IBL,
// and the only well-defined choice when light_count can be 0.
@fragment
fn fs_pbr(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = resolve_albedo(in.uv);
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    var N = resolve_normal(in.uv, in.world_normal);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    if dot(N, V) < 0.0 {
        N = -N;
    }
    let metallic = clamp(resolve_metallic(in.uv), 0.0, 1.0);
    let roughness = resolve_roughness(in.uv);

    let n_dot_v = max(dot(N, V), 0.001);
    let F0 = mix(vec3<f32>(0.04), albedo.rgb, metallic);
    let a = roughness * roughness;
    let a2 = a * a;
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    let g_v = n_dot_v / (n_dot_v * (1.0 - k) + k);

    var direct = vec3<f32>(0.0);
    let light_count = u32(u.scene_params.x);
    for (var i = 0u; i < light_count; i = i + 1u) {
        let l_dir = lights[i * 2u];
        let l_col = lights[i * 2u + 1u];
        let L = normalize(l_dir.xyz);
        let H = normalize(L + V);
        let n_dot_l = max(dot(N, L), 0.0);
        let n_dot_h = max(dot(N, H), 0.0);
        let v_dot_h = max(dot(V, H), 0.001);

        let denom_d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
        let D = a2 / (PI * denom_d * denom_d);

        let g_l = n_dot_l / (n_dot_l * (1.0 - k) + k);
        let G = g_v * g_l;

        let F = F0 + (1.0 - F0) * pow(clamp(1.0 - v_dot_h, 0.0, 1.0), 5.0);
        let specular = (D * G * F) / (4.0 * n_dot_v * n_dot_l + 0.0001);
        let kd = (1.0 - F) * (1.0 - metallic);
        let diffuse = kd * albedo.rgb / PI;

        let vis = shadow_factor(in.world_pos, l_col.w);
        direct = direct + (diffuse + specular) * l_col.rgb * n_dot_l * l_dir.w * vis;
    }

    let R = reflect(-V, N);
    let azimuth = atan2(R.z, R.x);
    let elevation = asin(clamp(R.y, -1.0, 1.0));
    let uv = vec2<f32>(azimuth / (2.0 * PI) + 0.5, elevation / PI + 0.5);
    let ibl_sample = textureSampleLevel(envmap, envmap_sampler, uv, 0.0).rgb;
    let ibl_strength = 1.0 - roughness * 0.7;
    let f_view = F0 + (1.0 - F0) * pow(clamp(1.0 - n_dot_v, 0.0, 1.0), 5.0);
    let ibl = f_view * ibl_sample * ibl_strength;

    let ambient = albedo.rgb * u.scene_params.y * u.ambient_tint.rgb;
    let rgb = apply_fog(direct + ibl + ambient + u.emission.rgb, in.world_pos);
    return vec4<f32>(rgb, albedo.a);
}

// Cel — Lambert N·L quantized into cel_bands discrete steps, summed over
// every wired light; ambient + emission added exactly once.
@fragment
fn fs_cel(in: VsOut) -> @location(0) vec4<f32> {
    let albedo = resolve_albedo(in.uv);
    if u.alpha_params.x == 1.0 && albedo.a < u.alpha_params.y {
        discard;
    }
    var N = resolve_normal(in.uv, in.world_normal);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    if dot(N, V) < 0.0 {
        N = -N;
    }
    let bands = max(u.cel_params.x, 2.0);
    let band_low = u.cel_params.y;
    let band_high = u.cel_params.z;

    var lit = vec3<f32>(0.0);
    let light_count = u32(u.scene_params.x);
    for (var i = 0u; i < light_count; i = i + 1u) {
        let l_dir = lights[i * 2u];
        let l_col = lights[i * 2u + 1u];
        let L = normalize(l_dir.xyz);
        let n_dot_l = max(dot(N, L), 0.0);
        let snapped = floor(n_dot_l * bands) / (bands - 1.0);
        let level = mix(band_low, band_high, clamp(snapped, 0.0, 1.0));
        let vis = shadow_factor(in.world_pos, l_col.w);
        lit = lit + albedo.rgb * level * l_col.rgb * l_dir.w * vis;
    }
    let ambient = albedo.rgb * u.scene_params.y * u.ambient_tint.rgb;
    let rgb = apply_fog(lit + ambient + u.emission.rgb, in.world_pos);
    return vec4<f32>(rgb, albedo.a);
}
