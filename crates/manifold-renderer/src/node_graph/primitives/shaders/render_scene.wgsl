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
// Shadows (P2) via the caster table + PCF below; atmosphere/fog (P3).
// Per-object surface textures (IMPORT_FIDELITY_DESIGN.md D3/F-P2):
// base_color_map (M6 addendum) plus normal_map (tangent-space, D4
// cotangent-frame reconstruction), mr_map (glTF-packed roughness/metallic),
// occlusion_map, and emissive_map — each gated by a texture_flags/
// texture_flags2 presence bit, unwired = always-bind dummy stub (P8
// pattern), byte-identical output. ONE envmap is shared across every PBR
// object in the scene (an environment map is scene-wide, not per-object).
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
// aligned throughout — every member is already a vec4/mat4 multiple, so no
// manual padding is needed. Total 480 bytes (four mat4x4s + fourteen vec4s;
// stale as "272"/"320"/"448"/"464" in older comments — grew with the P3
// atmosphere fields, the P2 prev_view_proj/prev_model pair, the
// VOLUMETRIC_LIGHT_DESIGN.md P1 shaft_params field, and
// IMPORT_FIDELITY_DESIGN.md D3/F-P2's texture_flags2;
// `RenderSceneUniforms`'s `size_of` assert in render_scene.rs is the
// authoritative check).
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
    // x: metallic [0,1], y: roughness [0.01,1]. z/w were permanently-zero
    // reserved slots until GLB_CONFORMANCE_DESIGN.md G-P4/D5 repurposed
    // them: z = ior (KHR_materials_ior, default 1.5), w = specular_factor
    // (KHR_materials_specular, default 1.0) — both feed fs_pbr's
    // dielectric F0 term below. Defaults collapse the formula to the
    // pre-G-P4 hardcoded F0 = 0.04 exactly.
    pbr_metallic_roughness: vec4<f32>,
    // rgb: specular tint, w: Phong exponent.
    specular: vec4<f32>,
    // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular's
    // specularColorFactor (rgb, default [1,1,1]), w reserved.
    pbr_specular_tint: vec4<f32>,
    // GLB_CONFORMANCE_DESIGN.md G-P4/D5: per-map KHR_texture_transform,
    // one folded 2×3 affine per map family. *_uv_m = linear part
    // (m00, m01, m10, m11) s.t. uv' = (m00*u + m01*v + tx, m10*u +
    // m11*v + ty), default identity (1,0,0,1); *_uv_t = translation
    // (tx, ty, 0, 0), z/w reserved, default zero. Per-map, not shared:
    // the AMG GT3 carries transforms on 9 normalTexture infos and only
    // 1 baseColorTexture.
    base_color_uv_m: vec4<f32>,
    base_color_uv_t: vec4<f32>,
    normal_uv_m: vec4<f32>,
    normal_uv_t: vec4<f32>,
    mr_uv_m: vec4<f32>,
    mr_uv_t: vec4<f32>,
    occlusion_uv_m: vec4<f32>,
    occlusion_uv_t: vec4<f32>,
    emissive_uv_m: vec4<f32>,
    emissive_uv_t: vec4<f32>,
    // x: cel_bands (as f32), y: band_low, z: band_high, w: reserved.
    cel_params: vec4<f32>,
    // Surface-texture presence flags. x: normal_map_n wired (D3/F-P2,
    // tangent-space glTF normal map — see resolve_normal's cotangent-frame
    // reconstruction below), y/w: reserved/unused (the old single-channel
    // roughness_map/metallic_map stubs are permanently dead — D3 rejected
    // reusing them with a channel-select mode flag; superseded by the
    // dedicated mr_map + texture_flags2.x below), z: base_color_map_n wired
    // (M6 addendum, matches resolve_albedo's gate).
    texture_flags: vec4<f32>,
    // IMPORT_FIDELITY_DESIGN.md D3/F-P2: texture_flags is full (see above),
    // so the three remaining new per-object maps get their own vec4.
    // x: mr_map_n wired (glTF packing: G=roughness, B=metallic — a
    // DEDICATED resolve function, not a channel-select mode on the old
    // roughness_map/metallic_map bindings, per D3's explicit rejection of
    // that shape). y: occlusion_map_n wired (R channel). z: emissive_map_n
    // wired (sRGB, multiplied by the material's emission factor). w:
    // reserved.
    texture_flags2: vec4<f32>,
    // x: alpha_mode (1.0 = Mask/cutout, 0.0 = Opaque), y: alpha_cutoff,
    // z/w: reserved.
    alpha_params: vec4<f32>,
    // x: light_count (as f32, unbounded — the `lights` storage buffer is
    // runtime-sized), y: ambient (this object's material.ambient),
    // z: exposure_ev (CAMERA_AND_LENS_DESIGN.md §2 D5 — the camera lens'
    // exposure_ev; every fragment entry multiplies its final straight rgb
    // by exp2(exposure_ev) just before returning), w: reserved.
    scene_params: vec4<f32>,
    // Atmosphere (P3), scene-wide (same in every object's uniform).
    // fog_color.rgb = colour distant geometry fades toward.
    fog_color: vec4<f32>,
    // x: fog_density (0 = no fog), y: height_falloff (0 = uniform),
    // z/w: reserved.
    fog_params: vec4<f32>,
    // rgb: ambient/sky tint multiplier on the ambient term (1,1,1 = neutral).
    ambient_tint: vec4<f32>,
    // VOLUMETRIC_LIGHT_DESIGN.md D1 (P1 plumbing only — no march kernel
    // reads this field yet, it is not consumed by any fragment entry point
    // below). x: shaft_intensity (0 = off, THE fader), y: shaft_anisotropy
    // (Henyey-Greenstein g), z: shaft_quality (0/1/2 = Low/Med/High, as
    // f32), w: reserved.
    shaft_params: vec4<f32>,
    // GBUFFER_DESIGN.md §2 D5 (P2): previous-frame scene view_proj and this
    // object's previous-frame model matrix — inputs to the EMIT_VELOCITY
    // pipeline variant's vs_main (clip_prev = prev_view_proj * prev_model *
    // position). Always present, ONE Uniforms layout shared by both the
    // velocity-on and velocity-off pipeline variants; the velocity-off
    // fragment/vertex code (the unmodified base file below) simply never
    // reads them. render_scene.rs seeds prev_model/prev_view_proj = this
    // frame's own model/view_proj on the very first evaluate() call (no
    // history yet), so first-frame velocity is exactly zero, not
    // approximately — see RenderScene::evaluate.
    prev_view_proj: mat4x4<f32>,
    prev_model: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> verts: array<Vertex>;
// PBR-only IBL envmap — ONE shared equirect map for every PBR object in
// the scene.
@group(0) @binding(2) var envmap: texture_2d<f32>;
@group(0) @binding(3) var envmap_sampler: sampler;
// Material maps sample with REPEAT on both axes (the glTF default sampler)
// — assets author UVs outside [0,1] freely (DamagedHelmet's V range is
// [1.0, 2.0]). envmap_sampler stays clamp-V for the equirect poles; sharing
// it here was the 2026-07-15 whole-helmet-smeared-to-the-texture-edge bug.
@group(0) @binding(22) var material_sampler: sampler;
// binding(4): normal_map_n (D3/F-P2) — tangent-space glTF normal map,
// gated by texture_flags.x, reconstructed via the screen-space cotangent
// frame in resolve_normal below (D4). Unwired binds the 1×1 dummy, flag
// stays 0 — always-bind stub pattern (render_scene.rs P8 precedent).
// binding(5)/(7): roughness_map/metallic_map — permanently dead stubs
// (D3 rejected a channel-select mode flag on these; superseded by the
// dedicated mr_map at binding(19) below). Always bound to the 1×1 dummy;
// texture_flags.y/w never set from Rust.
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
// [4] = (bias, kernel_half_width, texel_size, light_size). Filled only for
// active casters (zeroed otherwise); a light's caster slot rides
// lights[i*2+1].w (−1.0 = this light casts no shadow). The four shadow maps
// are separate bindings because WGSL has no dynamic texture-binding
// indexing — the K=4 switch in sample_shadow() picks the right one.
// `kernel_half_width` doubles as the PCSS dispatch: a NEGATIVE value (D12,
// REALTIME_3D_DESIGN §11) means this caster is the `Contact` softness tier
// — `shadow_factor` runs the PCSS blocker-search branch instead of the
// fixed kernel, and `light_size` (the 4th component, otherwise unused —
// "zero layout growth" per D12) drives both the blocker-search radius and
// the penumbra-width estimate.
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

// IMPORT_FIDELITY_DESIGN.md D2/F-P1: split-sum IBL. Always bound (same
// always-bind ABI-stub discipline as bindings 4/5/7/10-14 above) —
// `render_scene.rs::ensure_ibl_resources` guarantees all three exist
// regardless of whether `envmap` is wired; `fs_pbr` only actually samples
// them for PBR materials, which already require `envmap` wired.
// `PREFILTER_MAX_MIP` must stay in sync with the Rust-side
// `PREFILTER_MIP_COUNT - 1` (render_scene.rs) — same
// shared-compile-time-constant discipline as `CASTER_STRIDE`/
// `MAX_SHADOW_CASTING_LIGHTS` above.
const PREFILTER_MAX_MIP: f32 = 5.0;
@group(0) @binding(16) var prefiltered_specular: texture_2d<f32>;
@group(0) @binding(17) var irradiance_map: texture_2d<f32>;
@group(0) @binding(18) var brdf_lut: texture_2d<f32>;

// IMPORT_FIDELITY_DESIGN.md D3/F-P2: the three NEW per-object maps (normal
// reuses binding(4) above). Always bound (same always-bind ABI-stub
// discipline as bindings 4/5/7/10-14) — unwired binds the 1×1 dummy,
// gated off by texture_flags2's presence bits.
@group(0) @binding(19) var mr_map: texture_2d<f32>;
@group(0) @binding(20) var occlusion_map: texture_2d<f32>;
@group(0) @binding(21) var emissive_map: texture_2d<f32>;

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

// Plain (non-comparison) depth read for the PCSS blocker search (D12) —
// nearest texel, no hardware PCF. `texture_depth_2d` supports `textureLoad`
// directly (standard WGSL builtin), so this needs no second binding or
// sampler — the VERIFY-AT-IMPL in REALTIME_3D_DESIGN §11 resolves to "yes,
// via the existing binding": same four textures `sample_shadow` above
// already binds.
fn plain_shadow_depth(slot: i32, uv: vec2<f32>) -> f32 {
    let clamped = clamp(uv, vec2<f32>(0.0), vec2<f32>(0.999999));
    switch slot {
        case 0: {
            let px = vec2<i32>(clamped * vec2<f32>(textureDimensions(shadow_map_0)));
            return textureLoad(shadow_map_0, px, 0);
        }
        case 1: {
            let px = vec2<i32>(clamped * vec2<f32>(textureDimensions(shadow_map_1)));
            return textureLoad(shadow_map_1, px, 0);
        }
        case 2: {
            let px = vec2<i32>(clamped * vec2<f32>(textureDimensions(shadow_map_2)));
            return textureLoad(shadow_map_2, px, 0);
        }
        default: {
            let px = vec2<i32>(clamped * vec2<f32>(textureDimensions(shadow_map_3)));
            return textureLoad(shadow_map_3, px, 0);
        }
    }
}

// Shared (2·khw+1)² box-average PCF loop — byte-identical to the loop
// `shadow_factor` used to run inline. Both the fixed-kernel tiers
// (Hard/Soft/VerySoft) and the Contact tier's dynamic per-fragment width
// (D12 step 3, "the EXISTING PCF loop with half-width = ceil(penumbra_px)")
// go through this one function.
fn pcf_average(slot: i32, suv: vec2<f32>, ref_depth: f32, texel: f32, khw: i32) -> f32 {
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

const PCSS_TAPS: u32 = 16u;
const GOLDEN_ANGLE: f32 = 2.399963;

// Project a world point through the caster's light-space `vp` into shadow-
// map UV space — the same math `shadow_factor` runs for the fragment
// itself. `w <= 0.0` (behind the light) reads as "no offset" rather than a
// divide-by-negative, which only matters for the degenerate light_size=0
// callers below (a real occluded fragment is always in front of its own
// caster).
fn project_to_shadow_uv(vp: mat4x4<f32>, world_pos: vec3<f32>) -> vec2<f32> {
    let clip = vp * vec4<f32>(world_pos, 1.0);
    if clip.w <= 0.0 {
        return vec2<f32>(0.0);
    }
    let ndc = clip.xyz / clip.w;
    return vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
}

// D12's `light_size` is a world-units light diameter (the outer-card
// fader), but the blocker search and penumbra formula below both need a
// UV-space radius. The caster table carries no world-scale `range` field
// to convert one into the other without growing the ABI ("zero layout
// growth"), so this derives the conversion exactly, per-fragment, from the
// caster's own `vp` matrix instead: project `world_pos` offset by
// `light_size` along world X and world Z, and take the larger of the two
// resulting UV-space displacements. Two axes (not one) because a single
// offset axis can be near-parallel to the light's view direction for some
// light orientations, which would collapse to a near-zero UV shift and
// underestimate the true radius; X and Z can't both be degenerate for the
// same light. Exact for Sun's uniform-scale ortho frustum; a reasonable
// per-point approximation for Point's perspective one (same "v1
// approximation" spirit as `light.rs`'s single-face point-shadow doc
// comment).
fn pcss_search_radius_uv(vp: mat4x4<f32>, world_pos: vec3<f32>, suv: vec2<f32>, light_size: f32) -> f32 {
    let uv_x = project_to_shadow_uv(vp, world_pos + vec3<f32>(light_size, 0.0, 0.0));
    let uv_z = project_to_shadow_uv(vp, world_pos + vec3<f32>(0.0, 0.0, light_size));
    return max(length(uv_x - suv), length(uv_z - suv));
}

// D12 — PCSS contact-hardening penumbra (REALTIME_3D_DESIGN §11).
// (1) Blocker search: 16 golden-angle taps (CINEMATIC_POST D2 formula:
// r_i = sqrt((i+0.5)/N), theta_i = i·2.399963) in a `search_radius_uv`
// (the world-units `light_size` converted to UV space by
// `pcss_search_radius_uv`) radius, PLAIN depth reads (no hardware
// compare). Average the blockers' depth; zero blockers found = nothing
// occludes this point = fully lit, early out — no PCF sample needed.
// (2) penumbra_px = search_radius_uv·(z_r−z_b)/z_b, mapped to texels via
// `texel`, clamped [0, 24] — `search_radius_uv` stands in for the doc's
// `light_size` term here because it's already `light_size` converted
// through the exact same world→UV scale the search radius used, so the
// two stay consistent. (3) `pcf_average` above with
// half-width = ceil(penumbra_px).
// `rot` spins the golden-angle disc by a per-pixel angle (interleaved
// gradient noise from the fragment's screen position, computed once in
// `shadow_factor`). With an unrotated disc every pixel shares one tap
// pattern, which reads as visible banding/stippling across gentle penumbra
// gradients at 17 taps; rotating per pixel trades that structured artifact
// for fine unstructured noise the eye averages away. Applied to BOTH loops
// so the blocker search and the filter stay on the same disc.
fn pcss_shadow_factor(slot: i32, suv: vec2<f32>, ref_depth: f32, z_r: f32, search_radius_uv: f32, texel: f32, rot: f32) -> f32 {
    if search_radius_uv <= 0.0 {
        // D12 gate (b): Contact with light_size=0 must match the sharpest
        // fixed tier (Hard, kernel_half_width=1) within 1px of gradient
        // width. Reuse Hard's exact half-width so the two are
        // byte-identical, not just close.
        return pcf_average(slot, suv, ref_depth, texel, 1);
    }
    var blocker_sum = 0.0;
    var blocker_count = 0.0;
    for (var i: u32 = 0u; i < PCSS_TAPS; i = i + 1u) {
        let r = sqrt((f32(i) + 0.5) / f32(PCSS_TAPS));
        let theta = f32(i) * GOLDEN_ANGLE + rot;
        let tap_uv = suv + search_radius_uv * r * vec2<f32>(cos(theta), sin(theta));
        let d = plain_shadow_depth(slot, tap_uv);
        if d < ref_depth {
            blocker_sum = blocker_sum + d;
            blocker_count = blocker_count + 1.0;
        }
    }
    if blocker_count < 0.5 {
        return 1.0;
    }
    let z_b = blocker_sum / blocker_count;
    let penumbra_px = clamp(search_radius_uv * (z_r - z_b) / max(z_b, 1e-6) / texel, 0.0, 24.0);
    // Fixed tap count, variable radius (standard PCSS step 3). The original
    // step 3 fed penumbra_px into the dense (2·khw+1)² box above, which
    // grows to 49×49 = 2,401 compare taps per fragment at the 24px clamp —
    // ~250× the Hard tier's 9, and foliage scenes (occluders far above
    // their ground shadow) push nearly every fragment to that ceiling
    // (measured 5FPS on a tree GLB that runs 60FPS on Hard). The same
    // golden-angle disc as the blocker search, but with compare taps over
    // the penumbra radius, costs the same at every width. penumbra_px <= 1
    // keeps the shared 3×3 loop so near-contact regions stay byte-identical
    // to Hard — the same D12 gate (b) contract the light_size=0 early-out
    // above satisfies, here for the z_b → z_r limit.
    if penumbra_px <= 1.0 {
        return pcf_average(slot, suv, ref_depth, texel, 1);
    }
    let radius_uv = penumbra_px * texel;
    var sum = sample_shadow(slot, suv, ref_depth);
    for (var i: u32 = 0u; i < PCSS_TAPS; i = i + 1u) {
        let r = sqrt((f32(i) + 0.5) / f32(PCSS_TAPS));
        let theta = f32(i) * GOLDEN_ANGLE + rot;
        let tap_uv = suv + radius_uv * r * vec2<f32>(cos(theta), sin(theta));
        sum = sum + sample_shadow(slot, tap_uv, ref_depth);
    }
    return sum / (f32(PCSS_TAPS) + 1.0);
}

// Light visibility in [0,1]: 1 = fully lit, 0 = fully shadowed. `slot_f` is
// lights[i*2+1].w — negative means this light casts no shadow, so the point
// is always lit. Reconstructs the fragment's light-space position, then
// either runs the fixed (2·khw+1)² PCF kernel or (D12) the PCSS branch — a
// NEGATIVE `kernel_half_width` in the caster table is the Contact-tier
// sentinel (`render_scene.rs`'s caster-table build). Points outside the
// caster's frustum read as lit (no shadow data there) rather than
// clamped-dark.
fn shadow_factor(world_pos: vec3<f32>, slot_f: f32, frag_xy: vec2<f32>) -> f32 {
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
    let khw_raw = params.y;
    let texel = params.z;
    let light_size = params.w;

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

    if khw_raw < 0.0 {
        let search_radius_uv = pcss_search_radius_uv(vp, world_pos, suv, light_size);
        // Interleaved gradient noise (Jimenez 2014) on the fragment's
        // screen position: a well-distributed per-pixel angle in [0, 2π)
        // with no state and no texture read. Screen-space (not a
        // world_pos hash) so the noise density is uniform on screen
        // regardless of surface scale or viewing angle.
        let ign = fract(52.9829189 * fract(dot(frag_xy, vec2<f32>(0.06711056, 0.00583715))));
        return pcss_shadow_factor(slot, suv, ref_depth, ndc.z, search_radius_uv, texel, ign * 6.2831853);
    }
    return pcf_average(slot, suv, ref_depth, texel, i32(khw_raw));
}

// GBUFFER_DESIGN.md §2 D5, P2: the EMIT_VELOCITY pipeline variant's
// FsOut struct is text-substituted in here (replacing this comment) so
// fs_unlit/fs_phong/fs_pbr/fs_cel can return a second MRT output
// (`@location(1) velocity`) alongside `color`. Inert (a no-op comment) in
// the base/velocity-off compile — GBUFFER_FSOUT_VELOCITY_STRUCT is the
// substitution marker `render_scene.rs` targets.
// GBUFFER_FSOUT_VELOCITY_STRUCT

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // GBUFFER_DESIGN.md §2 D5, P2: EMIT_VELOCITY substitutes
    // `@location(3) clip_now: vec4<f32>, @location(4) clip_prev: vec4<f32>,`
    // here — the CURRENT and PREVIOUS clip-space positions, carried as
    // plain (perspective-correctly interpolated) varyings rather than
    // reusing `@builtin(position)` (whose fragment-stage value is already
    // viewport-transformed with `w` replaced by `1/w_clip` — not usable
    // for the NDC divide the velocity fragment code needs). Inert comment
    // in the velocity-off compile.
    // GBUFFER_VSOUT_VELOCITY_FIELDS
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
    // GBUFFER_DESIGN.md §2 D5, P2: EMIT_VELOCITY substitutes
    // `out.clip_now = out.clip_pos; let prev_world = u.prev_model *
    // vec4<f32>(inst_pos, 1.0); out.clip_prev = u.prev_view_proj *
    // prev_world;` here — reusing THIS frame's `inst_pos` (there is no
    // previous-frame instance buffer to diff against), so only the
    // object group's rigid `model_n` motion is captured; a moving
    // instance array (e.g. scatter_on_mesh re-scattering per frame)
    // contributes no per-instance velocity, the same documented v1
    // rigid-only-motion limitation D5 states for deform atoms. Inert
    // comment in the velocity-off compile.
    // GBUFFER_VS_VELOCITY_BODY
    out.world_normal = normalize((u.model * vec4<f32>(inst_normal, 0.0)).xyz);
    out.uv = v.uv;
    return out;
}

// IMPORT_FIDELITY_DESIGN.md D4/F-P2: tangent-space (glTF-convention) normal
// mapping via a screen-space cotangent frame (Mikkelsen's derivation, the
// technique three.js/filament use when no vertex tangents exist — see
// MeshVertex's 48-byte ABI-pinned comment: growing it for tangents was
// priced and rejected). Built purely from `dpdx`/`dpdy` of `world_pos` and
// `uv` — both screen-space derivatives, so the reconstructed T/B are a
// function of the surface's own UV parameterization, independent of camera
// or screen resolution. Uniform per-object branch (texture_flags.x is a
// per-draw-call uniform, not per-fragment data), so `dpdx`/`dpdy` inside the
// `if` are legal (same discipline the PCSS branches above already rely on).
fn cotangent_frame(n: vec3<f32>, p: vec3<f32>, uv: vec2<f32>) -> mat3x3<f32> {
    let dp1 = dpdx(p);
    let dp2 = dpdy(p);
    let duv1 = dpdx(uv);
    let duv2 = dpdy(uv);

    let dp2perp = cross(dp2, n);
    let dp1perp = cross(n, dp1);
    let t = dp2perp * duv1.x + dp1perp * duv2.x;
    let b = dp2perp * duv1.y + dp1perp * duv2.y;

    let inv_max = inverseSqrt(max(dot(t, t), dot(b, b)));
    return mat3x3<f32>(t * inv_max, b * inv_max, n);
}

// Identical to render_3d_mesh.wgsl's SIGNATURE — see that file for the
// world-space-map path it still uses. render_scene's normal_map_n (D3) is
// tangent-space (glTF convention: R/G = tangent-space X/Y in [-1,1] packed
// to [0,1], B = tangent-space Z), reconstructed into world space via the
// cotangent frame above rather than added directly to the vertex normal.
fn resolve_normal(uv: vec2<f32>, vertex_normal: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    if u.texture_flags.x > 0.5 {
        let n = normalize(vertex_normal);
        // G-P4: per-map KHR_texture_transform. The cotangent frame is
        // built from the SAME transformed UV the texture is sampled with
        // — a rotated/scaled UV space rotates/scales the tangent
        // directions, and deriving T/B from the untransformed uv would
        // bend the decoded normals off-axis. Identity transform makes
        // uv_t == uv bit-for-bit (1*u + 0*v + 0), so pre-G-P4 assets are
        // byte-identical.
        let uv_t = apply_uv_transform(uv, u.normal_uv_m, u.normal_uv_t);
        let sampled = textureSample(normal_map, material_sampler, uv_t).rgb;
        let tangent_normal = sampled * 2.0 - vec3<f32>(1.0);
        let tbn = cotangent_frame(n, world_pos, uv_t);
        return normalize(tbn * tangent_normal);
    }
    return normalize(vertex_normal);
}

// GLB_CONFORMANCE_DESIGN.md G-P4/D5: apply the base-color
// KHR_texture_transform's folded affine — `uv' = M*uv + t`, folded ONCE at
// import time (gltf_load::fold_uv_transform), never per frame. Identity
// (m=(1,0,0,1), t=(0,0)) reduces to `uv` exactly (no branch needed: 1*u +
// 0*v + 0 == u bit-for-bit in f32).
fn apply_uv_transform(uv: vec2<f32>, m: vec4<f32>, t: vec4<f32>) -> vec2<f32> {
    return vec2<f32>(
        m.x * uv.x + m.y * uv.y + t.x,
        m.z * uv.x + m.w * uv.y + t.y,
    );
}

fn resolve_albedo(uv: vec2<f32>) -> vec4<f32> {
    if u.texture_flags.z > 0.5 {
        let uv_t = apply_uv_transform(uv, u.base_color_uv_m, u.base_color_uv_t);
        let t = textureSample(base_color_map, material_sampler, uv_t);
        return vec4<f32>(u.base_color.rgb * t.rgb, u.base_color.a * t.a);
    }
    return u.base_color;
}

// IMPORT_FIDELITY_DESIGN.md D3/F-P2: glTF metallic-roughness packing
// (G = roughness, B = metallic) in ONE dedicated texture — NOT a
// channel-select mode on the (now-dead) roughness_map/metallic_map
// bindings, per D3's explicit rejection of that shape. Returns
// (roughness, metallic).
fn resolve_mr(uv: vec2<f32>) -> vec2<f32> {
    if u.texture_flags2.x > 0.5 {
        let uv_t = apply_uv_transform(uv, u.mr_uv_m, u.mr_uv_t);
        let t = textureSample(mr_map, material_sampler, uv_t);
        return vec2<f32>(max(t.g, 0.01), clamp(t.b, 0.0, 1.0));
    }
    return vec2<f32>(max(u.pbr_metallic_roughness.y, 0.01), clamp(u.pbr_metallic_roughness.x, 0.0, 1.0));
}

// IMPORT_FIDELITY_DESIGN.md D3/F-P2: R-channel ambient occlusion. Unwired
// = 1.0 (no darkening) — used ONLY to darken fs_pbr's diffuse IBL term
// (never direct lighting, never specular IBL), per the design's Invariants
// table.
fn resolve_occlusion(uv: vec2<f32>) -> f32 {
    if u.texture_flags2.y > 0.5 {
        let uv_t = apply_uv_transform(uv, u.occlusion_uv_m, u.occlusion_uv_t);
        return textureSample(occlusion_map, material_sampler, uv_t).r;
    }
    return 1.0;
}

// IMPORT_FIDELITY_DESIGN.md D3/F-P2: sRGB emissive map, multiplied by the
// material's own (already premultiplied-with-intensity) emission factor.
// Unwired = the material's emission factor alone (byte-identical to before
// this port existed). Used in EVERY entry point (fs_unlit included, per
// M6-D1's albedo precedent) — emission is always added AFTER lighting.
fn resolve_emissive(uv: vec2<f32>) -> vec3<f32> {
    if u.texture_flags2.z > 0.5 {
        let uv_t = apply_uv_transform(uv, u.emissive_uv_m, u.emissive_uv_t);
        let t = textureSample(emissive_map, material_sampler, uv_t).rgb;
        return u.emission.rgb * t;
    }
    return u.emission.rgb;
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
    // exp2(exposure_ev) — CAMERA_AND_LENS_DESIGN.md §2 D5. Multiplies the
    // final STRAIGHT rgb (post-fog, post-emission, pre-output); alpha is
    // untouched (alpha contract). exposure_ev = 0 (PINHOLE default) → ×1,
    // byte-identical to pre-lens builds (I2/I5).
    let rgb = apply_fog(albedo.rgb + resolve_emissive(in.uv), in.world_pos) * exp2(u.scene_params.z);
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
    var N = resolve_normal(in.uv, in.world_normal, in.world_pos);
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
        let vis = shadow_factor(in.world_pos, l_col.w, in.clip_pos.xy);
        lit = lit + (diffuse + spec) * l_col.rgb * l_dir.w * vis;
    }
    let ambient = albedo.rgb * u.scene_params.y * u.ambient_tint.rgb;
    // exp2(exposure_ev) — CAMERA_AND_LENS_DESIGN.md §2 D5, see fs_unlit.
    let rgb = apply_fog(lit + ambient + resolve_emissive(in.uv), in.world_pos) * exp2(u.scene_params.z);
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
    var N = resolve_normal(in.uv, in.world_normal, in.world_pos);
    let V = normalize(u.camera_pos.xyz - in.world_pos);
    if dot(N, V) < 0.0 {
        N = -N;
    }
    let mr = resolve_mr(in.uv);
    let roughness = mr.x;
    let metallic = mr.y;

    let n_dot_v = max(dot(N, V), 0.001);
    // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular + ior →
    // F0 scale. Verified against the Khronos KHR_materials_specular
    // extension README (not the design doc's own draft formula, which
    // omitted specularFactor and carried a spurious 0.16 constant — see
    // the G-P4 execution report):
    //   dielectric_f0 = min(((ior-1)/(ior+1))^2 * specularColorFactor, 1.0)
    //                   * specularFactor
    // Defaults (ior=1.5, specular_factor=1.0, specular_tint=(1,1,1))
    // reduce this to exactly (0.04, 0.04, 0.04) — the pre-G-P4 hardcoded
    // dielectric baseline. v1 scope: F0 only (dielectric_f90 stays 1.0,
    // i.e. the Schlick term below still assumes a white grazing edge —
    // KHR_materials_specular also modulates f90 by specular_factor, which
    // this phase's brief scoped OUT ("map to F0 scale"); a specular_factor
    // of 0 dims but does not zero the grazing reflection, a known v1
    // limitation).
    let ior = u.pbr_metallic_roughness.z;
    let specular_factor = u.pbr_metallic_roughness.w;
    let specular_tint = u.pbr_specular_tint.rgb;
    let dielectric_reflectance = pow((ior - 1.0) / (ior + 1.0), 2.0);
    let dielectric_f0 = min(dielectric_reflectance * specular_tint, vec3<f32>(1.0)) * specular_factor;
    let F0 = mix(dielectric_f0, albedo.rgb, metallic);
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

        let vis = shadow_factor(in.world_pos, l_col.w, in.clip_pos.xy);
        direct = direct + (diffuse + specular) * l_col.rgb * n_dot_l * l_dir.w * vis;
    }

    // Split-sum IBL (IMPORT_FIDELITY_DESIGN.md D2/F-P1): prefiltered
    // specular reflection (mip selected by roughness) combined with the
    // BRDF LUT's scale/bias, plus cosine-convolved diffuse irradiance —
    // replaces the old single lod-0 envmap sample + roughness-fade
    // heuristic entirely.
    let R = reflect(-V, N);
    let r_azimuth = atan2(R.z, R.x);
    let r_elevation = asin(clamp(R.y, -1.0, 1.0));
    let r_uv = vec2<f32>(r_azimuth / (2.0 * PI) + 0.5, r_elevation / PI + 0.5);
    let prefiltered = textureSampleLevel(prefiltered_specular, envmap_sampler, r_uv, roughness * PREFILTER_MAX_MIP).rgb;

    let n_azimuth = atan2(N.z, N.x);
    let n_elevation = asin(clamp(N.y, -1.0, 1.0));
    let n_uv = vec2<f32>(n_azimuth / (2.0 * PI) + 0.5, n_elevation / PI + 0.5);
    let irradiance = textureSampleLevel(irradiance_map, envmap_sampler, n_uv, 0.0).rgb;

    let env_brdf = textureSampleLevel(brdf_lut, envmap_sampler, vec2<f32>(n_dot_v, roughness), 0.0).rg;
    let specular_ibl = prefiltered * (F0 * env_brdf.x + env_brdf.y);

    // View-angle Fresnel splits IBL energy between specular and diffuse —
    // the standard split-sum substitute for the light-dependent N·H term a
    // single direct light would give, and the only well-defined choice
    // when light_count can be 0 (same reasoning the deleted roughness-fade
    // heuristic's neighbouring comment already documented for this split).
    let f_view = F0 + (1.0 - F0) * pow(clamp(1.0 - n_dot_v, 0.0, 1.0), 5.0);
    let kd_ibl = (1.0 - f_view) * (1.0 - metallic);
    // IMPORT_FIDELITY_DESIGN.md D3/F-P2: occlusion darkens the diffuse IBL
    // term ONLY — never direct lighting (the `direct` accumulator above),
    // never specular IBL (`specular_ibl`) — per the design's Invariants
    // table. Unwired = 1.0 (no darkening), byte-identical to before this
    // port existed.
    let occlusion = resolve_occlusion(in.uv);
    let diffuse_ibl = kd_ibl * albedo.rgb * irradiance * occlusion;

    let ibl = specular_ibl + diffuse_ibl;

    let ambient = albedo.rgb * u.scene_params.y * u.ambient_tint.rgb;
    // exp2(exposure_ev) — CAMERA_AND_LENS_DESIGN.md §2 D5, see fs_unlit.
    let rgb = apply_fog(direct + ibl + ambient + resolve_emissive(in.uv), in.world_pos) * exp2(u.scene_params.z);
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
    var N = resolve_normal(in.uv, in.world_normal, in.world_pos);
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
        let vis = shadow_factor(in.world_pos, l_col.w, in.clip_pos.xy);
        lit = lit + albedo.rgb * level * l_col.rgb * l_dir.w * vis;
    }
    let ambient = albedo.rgb * u.scene_params.y * u.ambient_tint.rgb;
    // exp2(exposure_ev) — CAMERA_AND_LENS_DESIGN.md §2 D5, see fs_unlit.
    let rgb = apply_fog(lit + ambient + resolve_emissive(in.uv), in.world_pos) * exp2(u.scene_params.z);
    return vec4<f32>(rgb, albedo.a);
}
