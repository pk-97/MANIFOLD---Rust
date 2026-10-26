// Scene shading shared by every pass that lights a scene surface: the
// object uniform, lights, shadows, image-based lighting, the ray-traced
// substitutions, fog and the PBR light loop. It holds no vertex stage and no
// fragment entry point, so a full-screen pass can prepend it as well as
// `render_scene.wgsl`. Prepended with `concat!` (render_scene.rs); WGSL module
// scope is order-free. Fragment entry points stay out of here: the pipeline
// variants rewrite the return type of every fragment entry in the composed
// text.

const PI: f32 = 3.14159265358979;

// Superset uniform, rebuilt once per object per draw call. 16-byte
// aligned throughout — every member is already a vec4/mat4 multiple, so no
// manual padding is needed. Total 800 bytes (four mat4x4s + thirty-four
// vec4s; stale as "272"/"320"/"448"/"464"/"480" in older comments — grew
// with the P3 atmosphere fields, the P2 prev_view_proj/prev_model pair, the
// Shaft_params field,
// texture_flags2, and
// the five-vec4 material-extension
// block at the end;
// `RenderSceneUniforms`'s `size_of` assert in render_scene.rs is the
// authoritative check).
// Lights are NO LONGER in here: they live in the `@binding(8)` storage
// buffer below (a scene can carry any number of lights, not the old 4).
struct Uniforms {
    view_proj: mat4x4<f32>,
    // This object's world transform, composed CPU-side from
    // pos_x/y/z + rot_x/y/z (Euler, X→Y→Z order, matching
    // render_instanced_3d_mesh.wgsl's euler_xyz) + scale_x/y/z.
    // Normals use the inverse transpose; tangent directions use its linear part.
    model: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // rgb: surface diffuse / base colour, w: opacity (informational).
    base_color: vec4<f32>,
    // rgb: emission PREMULTIPLIED with intensity; w: per-object emissive-map
    // strength multiplier (defaults to 1.0, so existing materials are unchanged).
    emission: vec4<f32>,
    // x: metallic [0,1], y: roughness [0.01,1]. z/w were permanently-zero
    // reserved slots until they were repurposed
    // them: z = ior (KHR_materials_ior, default 1.5), w = specular_factor
    // (KHR_materials_specular, default 1.0) — both feed fs_pbr's
    // dielectric F0 term below. Defaults collapse the formula to the
    // pre-G-P4 hardcoded F0 = 0.04 exactly.
    pbr_metallic_roughness: vec4<f32>,
    // Retired legacy lighting slot, kept reserved for uniform ABI stability.
    specular: vec4<f32>,
    // KHR_materials_specular's
    // specularColorFactor (rgb, default [1,1,1]). w: GLTF_MATERIAL_
    // EXTENSIONS_DESIGN.md E6 bitmask (see specular_family_flags()) —
    // bit0=specular_map, bit1=specular_color_map, bit2=transmission_map,
    // bit3=volume_thickness_map present.
    pbr_specular_tint: vec4<f32>,
    // Per-map KHR_texture_transform,
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
    // texture_flags is full (see above),
    // so the three remaining new per-object maps get their own vec4.
    // x: mr_map_n wired (glTF packing: G=roughness, B=metallic — a
    // DEDICATED resolve function, not a channel-select mode on the old
    // roughness_map/metallic_map bindings, per D3's explicit rejection of
    // that shape). y: occlusion_map_n wired (R channel). z: emissive_map_n
    // wired (sRGB, multiplied by the material's emission factor). w:
    // reserved.
    texture_flags2: vec4<f32>,
    // x: alpha_mode (1.0 = Mask/cutout, 0.0 = Opaque), y: alpha_cutoff.
    // z/w were permanently-zero reserved slots until
    // Repurposed as z = clearcoat
    // (KHR_materials_clearcoat's clearcoatFactor, default 0.0), w =
    // clearcoat_roughness (clearcoatRoughnessFactor, default 0.0) — both
    // feed fs_pbr's second GGX lobe below. Default 0.0 makes the energy-
    // compensation weight exactly zero, byte-identical to pre-G-P5 output.
    alpha_params: vec4<f32>,
    // x: light_count (as f32, unbounded — the `lights` storage buffer is
    // runtime-sized), y: ambient (this object's material.ambient),
    // z: exposure_ev (the camera lens'
    // exposure_ev; every fragment entry multiplies its final straight rgb
    // by exp2(exposure_ev) just before returning), w: reserved.
    scene_params: vec4<f32>,
    // Atmosphere (P3), scene-wide (same in every object's uniform).
    // fog_color.rgb = colour distant geometry fades toward.
    fog_color: vec4<f32>,
    // x: fog_density (0 = no fog), y: height_falloff (0 = uniform),
    // z/w: reserved.
    fog_params: vec4<f32>,
    // x: appearance gain, y: 1 when per-vertex weights are wired, z: the
    // single-object legacy map bits, w: the envmap's turn in radians
    // (`env_lookup_uv`).
    appearance: vec4<f32>,
    // rgb: ambient/sky tint multiplier on the ambient term (1,1,1 = neutral).
    ambient_tint: vec4<f32>,
    // Plumbing only — no march kernel
    // reads this field yet, it is not consumed by any fragment entry point
    // below). x: shaft_intensity (0 = off, THE fader), y: shaft_anisotropy
    // (Henyey-Greenstein g), z: shaft_quality (0/1/2 = Low/Med/High, as
    // f32), w: reserved.
    shaft_params: vec4<f32>,
    // Previous-frame scene view_proj and this
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
    // Single aligned block, grown
    // ONCE, sized for ALL FIVE families the design's phases add (sheen,
    // iridescence, anisotropy, dispersion, transmission+volume) — see
    // `RenderSceneUniforms`'s matching field doc comments in render_scene.rs
    // for the authoritative packing rationale. NOT read by any fragment
    // entry point below yet (E2-E6 wire that) — declared here only so the
    // buffer size matches `RenderSceneUniforms`'s `size_of` assert.
    // KHR_materials_sheen: xyz = sheenColorFactor (default [0,0,0], inert),
    // w = sheenRoughnessFactor (default 0.0).
    sheen_params: vec4<f32>,
    // KHR_materials_iridescence: x = iridescenceFactor (default 0.0,
    // inert), y = iridescenceIor (default 1.3), z =
    // iridescenceThicknessMinimum (default 100.0 nm), w =
    // iridescenceThicknessMaximum (default 400.0 nm).
    iridescence_params: vec4<f32>,
    // KHR_materials_anisotropy's anisotropyStrength (x, default 0.0) +
    // anisotropyRotation (y, default 0.0 rad); KHR_materials_dispersion's
    // single factor (z, default 0.0) shares this vec4 rather than costing
    // its own. w reserved.
    anisotropy_dispersion_params: vec4<f32>,
    // KHR_materials_transmission's transmissionFactor (x, default 0.0) +
    // KHR_materials_volume's thicknessFactor (y, default 0.0) and
    // attenuationDistance (z, default a finite "no attenuation" sentinel —
    // see gltf_load.rs's VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION). w
    // reserved.
    transmission_volume_params: vec4<f32>,
    // KHR_materials_volume's attenuationColor (xyz, default [1,1,1] —
    // neutral). w: bitmask (see
    // clearcoat_family_flags()) — bit0=clearcoat_map, bit1=
    // clearcoat_roughness_map, bit2=clearcoat_normal_map present.
    volume_attenuation_color: vec4<f32>,
    // RAYTRACING_DESIGN.md section 16 TL7: KHR_materials_diffuse_transmission.
    // x = diffuse transmission factor (default 0.0 — inert). yzw reserved
    // (first consumer = the future color-texture flag, section 16.8). Mirrors
    // `RenderSceneUniforms::diffuse_transmission_params` in render_scene.rs.
    diffuse_transmission_params: vec4<f32>,
    // RAYTRACING_DESIGN.md section 9 RD9/RD1: RT feature flags. x =
    // rt_reflections active this frame (rt_enabled && rt_ready &&
    // rt_reflections && non-empty casters — the trace dispatch ran with
    // refl_spp > 0, so binding 43 holds traced data). y = the traced-
    // diffuse substitution gate (section 14 ED6: the GI gather ran with
    // gi_spp > 0, so binding 42's `.rgb` holds env+GI radiance). z =
    // designated sun caster slot for svt substitution (TL5). w =
    // RT shadow mask read gate (rt_shadows toggle).
    rt_flags: vec4<f32>,
    // TAA/MetalFX velocity jitter exclusion: (cur_x, cur_y, prev_x,
    // prev_y) as NDC offsets (jitter_px × 2/dim). MetalFX expects motion
    // vectors WITHOUT camera jitter (its jitterOffset compensates the
    // current frame's jitter itself); clip_now/clip_prev both carry it
    // baked in, so the velocity fragment subtracts `xy - zw`. All zeros
    // when temporal_upscale is off — velocity byte-identical to before.
    velocity_jitter: vec4<f32>,
    // SCENE_RENDER_MODE_DESIGN.md D8 (P3): render-mode params, scene-wide.
    // x = point_size (px, read by vs_points ONLY — every other vertex entry
    // ignores it, so Rendered/Solid/Wireframe are byte-identical with any
    // value here). y = mip_bias (log2(render_w / native_w) when temporal_upscale
    // is on, 0.0 when off). z = 1.0 when `prev_positions` holds last frame's
    // position for every vertex this draw reads (a mesh that deforms with the
    // same triangles), else 0.0; per draw, read by the velocity variant only.
    // w = 1.0 when `prev_instances` holds last frame's record for every
    // instance this draw reads (an instance array that moves on its own), else
    // 0.0; per draw, read by the velocity variant only.
    render_mode: vec4<f32>,
    volume_optics: vec4<f32>,
    volume_scattering_color: vec4<f32>,
    // The instance space: world = model · T_instance · instance_inner.
    // Identity in Object space; the object's own transform in Group space.
    instance_inner: mat4x4<f32>,
    prev_instance_inner: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// PBR-only IBL envmap — ONE shared equirect map for every PBR object in
// the scene.
@group(0) @binding(2) var envmap: texture_2d<f32>;
@group(0) @binding(3) var envmap_sampler: sampler;

// The equirect uv the envmap family (prefiltered specular and sheen,
// irradiance) is read at for a world direction, turned by the Environment's
// rotation (`u.appearance.w`, radians, counter-clockwise seen from above).
// The maps stay baked unturned, so a live turn costs nothing. u may leave
// [0, 1]; the envmap sampler repeats in u. `camera_sky_body.wgsl` and the
// ray-traced lookups (`refl_env_sample`) turn the same way.
fn env_lookup_uv(dir: vec3<f32>) -> vec2<f32> {
    return vec2<f32>((atan2(dir.z, dir.x) + u.appearance.w) / (2.0 * PI) + 0.5,
        asin(clamp(dir.y, -1.0, 1.0)) / PI + 0.5);
}

// Scene lights, 4 vec4s each, runtime-sized (was a fixed `array<vec4,8>`
// inside Uniforms; the old cap of 4 lights is gone). Only the first
// `scene_params.x` entries are meaningful — count flows through the
// uniform, NOT `arrayLength` (D1). At zero lights the CPU still binds one
// zeroed entry so Metal always sees a bound buffer (D4).
// Same layout as shaft_lights: Sun direction is toward the light.
//   lights[i*4]   = (Sun: direction / Point/Spot: position, w: mode 0/1/2)
//   lights[i*4+1] = (colour.rgb premultiplied with intensity, w: caster slot)
//   lights[i*4+2] = (range, falloff 0/1, cos(inner), cos(outer))
//   lights[i*4+3] = (Spot forward direction, w: 0)
@group(0) @binding(8) var<storage, read> lights: array<vec4<f32>>;
const LIGHT_STRIDE: u32 = 4u;

// Same Point meaning as Light::light_dir_at/attenuation_at and shaft_march:
// return direction toward the light in xyz and distance attenuation in w.
fn light_direction_attenuation(index: u32, world_pos: vec3<f32>) -> vec4<f32> {
    let base = index * LIGHT_STRIDE;
    let pos_or_dir = lights[base];
    if pos_or_dir.w < 0.5 {
        return vec4<f32>(normalize(pos_or_dir.xyz), 1.0);
    }
    let delta = pos_or_dir.xyz - world_pos;
    let d_sq = dot(delta, delta);
    let falloff = lights[base + 2u].y;
    let range = lights[base + 2u].x;
    let direction = select(
        vec3<f32>(0.0, 0.0, 1.0),
        delta * inverseSqrt(max(d_sq, 1e-20)),
        d_sq >= 1e-20,
    );
    let distance = sqrt(d_sq);
    let r_sq = range * range;
    var attenuation = 1.0 / (1.0 + d_sq / max(r_sq, 1e-10));
    if falloff > 0.5 {
        attenuation = select(
            (1.0 - pow(distance / max(range, 1e-6), 4.0)) / max(d_sq, 1e-6),
            1.0 / max(d_sq, 1e-6),
            range <= 0.0,
        );
        if range > 0.0 {
            attenuation = max(1.0 - pow(distance / range, 4.0), 0.0) / max(d_sq, 1e-6);
        }
    } else {
        attenuation = select(attenuation, 0.0, r_sq < 1e-10);
    }
    if pos_or_dir.w > 1.5 {
        let cone = lights[base + 2u];
        let forward = lights[base + 3u].xyz;
        let cone_factor = clamp(
            (dot(forward, -direction) - cone.w) / max(cone.z - cone.w, 0.001),
            0.0,
            1.0,
        );
        attenuation = attenuation * cone_factor * cone_factor;
    }
    return vec4<f32>(direction, attenuation);
}

// One entry per shadow-casting light, shared with the ray tracer; a
// light's caster slot rides lights[i*4+1].w (-1.0 = no shadow). The struct
// text is generated from manifold-gpu's Rust `ShadowCaster` and a test keeps
// this copy identical. `shadow` = (bias, kernel_half_width, texel_size,
// light_size): a NEGATIVE `kernel_half_width` (D12, REALTIME_3D_DESIGN
// section 11) is the `Contact` tier — `shadow_factor` runs the PCSS
// blocker search, with `light_size` driving the search radius and the
// penumbra estimate.
struct ShadowCaster {
    view_proj: mat4x4<f32>,
    shadow: vec4<f32>,
    source: vec4<f32>,
    color: vec4<f32>,
}
@group(0) @binding(9) var<storage, read> casters: array<ShadowCaster>;
// One shadow-map layer per caster slot. Every layer has the largest
// requested resolution; a caster asking for less renders into the top-left
// 1/texel_size square of its layer.
@group(0) @binding(10) var shadow_maps: texture_depth_2d_array;
@group(0) @binding(14) var shadow_sampler: sampler_comparison;

// Split-sum IBL. Always bound (same
// always-bind ABI-stub discipline as bindings 4/5/7/10-14 above) —
// `render_scene.rs::ensure_ibl_resources` guarantees all three exist
// regardless of whether `envmap` is wired; `fs_pbr` only actually samples
// them for PBR materials, which already require `envmap` wired.
// `PREFILTER_MAX_MIP` must stay in sync with the Rust-side
// `PREFILTER_MIP_COUNT - 1` (render_scene.rs).
const PREFILTER_MAX_MIP: f32 = 5.0;
// RAYTRACING_DESIGN.md section 14 ED2: the flat ambient's knob-at-1
// ceiling — mirrors `render_scene.rs`'s `AMBIENT_IRRADIANCE_SCALE` (0.15).
// Same cross-mirror discipline as `PREFILTER_MAX_MIP`/`RT_REFL_PREFILTER_MAX_MIP`.
const AMBIENT_IRRADIANCE_SCALE: f32 = 0.15;
@group(0) @binding(16) var prefiltered_specular: texture_2d<f32>;
// `irradiance_map`: the raster cosine-convolved diffuse irradiance map —
// sampled EXACTLY ONCE in `fs_pbr`, at the `diffuse_ibl` substitution site
// (I-ED2). When the GI gather ran (ED6, `rt_flags.y > 0.5`) the traced
// `.rgb` SUBSTITUTES for this fetch — the same physical quantity, same
// scale (`ibl_irradiance.wgsl`'s estimator and the gather are identical).
@group(0) @binding(17) var irradiance_map: texture_2d<f32>;
@group(0) @binding(18) var brdf_lut: texture_2d<f32>;

// The opaque-scene-color snapshot (the fully shaded opaque result, blitted
// between the opaque and blend passes) that transmissive surfaces refract.
// Its sampler also reads the BRDF LUT.
@group(0) @binding(27) var opaque_scene_color: texture_2d<f32>;
@group(0) @binding(28) var opaque_scene_color_sampler: sampler;

// RAYTRACING_DESIGN.md RT-D3: full-res RT per-caster shadow-visibility
// mask, written by manifold-gpu's `ShadowRayTracer` half-res dispatch +
// upsample. `shadow_factor` below samples this via `textureLoad` at the
// exact fragment pixel (already native-res, depth-aware-upsampled — no
// filtering) instead of the shadow-map path when `u.scene_params.w > 0.5`.
// Always bound (ABI-stub discipline); a 1x1 dummy when RT isn't active
// this frame. Caster slot c is channel c % 4 of layer c / 4 (the same slot
// index the caster table uses). AO does not ride this texture
// at all — it's folded into the RT irradiance term (`rt_irradiance_mask`
// below) in-kernel.
@group(0) @binding(41) var rt_shadow_mask: texture_2d_array<f32>;
// RAYTRACING_DESIGN.md section 5.2 P2/D3, ED2 (section 14.2): full-res,
// temporally-accumulated demodulated irradiance — `.rgb` = the env+GI
// gather (the flat ambient no longer enters the kernel), `.a` = the
// accumulated ao, carried through the whole accumulation chain. Written
// by `trace_shadow_rays` + `accumulate_irradiance`. Always bound (ABI-stub
// discipline); a 1x1 dummy when RT isn't active this frame. Read in
// exactly TWO places (I-ED2): the `diffuse_ibl` substitution (`.rgb`) and
// `rt_or_flat_ambient` (`.a`).
@group(0) @binding(42) var rt_irradiance_mask: texture_2d<f32>;
// RAYTRACING_DESIGN.md section 9 RD1: full-res traced reflection radiance,
// SUBSTITUTED for the `prefiltered` env fetch in fs_pbr when
// `scene_params.w > 0.5 && rt_flags.x > 0.5` — never added on top
// (the 818a06b0 double-count trap). Always bound (ABI-stub discipline —
// a 1x1 dummy when RT reflections are off this frame), exactly like
// rt_irradiance_mask above. Consumed in exactly ONE place (I-R3).
@group(0) @binding(43) var rt_reflection: texture_2d<f32>;
// RT-TL-C (RAYTRACING_DESIGN.md section 16 TL5): full-res rgb
// sun-transmission tint for the ONE designated sun caster (rt_flags.z =
// slot+1, 0 = none). Always bound; 1x1 dummy when RT off, no designated
// sun, or the shadow kernel off (rt_flags.z is 0 in all three — only a
// shadow_spp > 0 dispatch writes this texture, BUG-majv). Consumed in
// exactly ONE textureLoad (I-TL3) in fs_pbr's light loop.
@group(0) @binding(45) var rt_sun_tint: texture_2d<f32>;

// Sheen's prefiltered environment (same mips as `prefiltered_specular`).
@group(0) @binding(49) var prefiltered_sheen: texture_2d<f32>;

// RAYTRACING_DESIGN.md section 5.2 P2: RT ambient/AO term. Replaces the flat
// `scene_params.y` ambient scalar with the ray-traced AO-occluded,
// temporally-accumulated demodulated irradiance when RT is on — this IS
// the structural replacement for a downstream `node.ssao_gtao` Multiply
// node (RAYTRACING_DESIGN.md section 5.2 P2's "GTAO term replaced, not
// paralleled" deliverable): an RT-enabled `render_scene` never needs
// (and should not be wired to) a separate GTAO node, since real
// ray-traced occlusion is already folded in here. Falls back to the
// original flat-ambient formula when RT is off — byte-identical to
// before this function existed (I2/I5).
//
// ED2 (section 14.2): the RT branch recomposes today's flat-ambient value
// consumer-side — `albedo * scene_params.y * ambient_tint *
// AMBIENT_IRRADIANCE_SCALE * mask.a`. The kernel's irradiance texture no
// longer folds the ambient in; each term is consumed in exactly one place,
// and the Ambient knob never gets `kd_ibl` scaling. `.a` of the traced
// texture is the accumulated ao (the kernel write sites carry it through
// the same weights). RT-off remains `albedo * scene_params.y *
// ambient_tint.rgb` — the Ambient knob is the flat fill on both paths.
//
// BUG-majv: the RT branch also requires `fog_params.w` (rt_ao_live) — the
// irradiance texture's `.a` is only written when the trace dispatch ran
// with ao_spp > 0 (gi_spp > 0 writes a neutral 1.0, but ao-only-off means
// the AO term is the raster's job again). Gating on scene_params.w alone
// read a stale/zeroed `.a` with the AO kernel off — after an RT off->on
// cycle that zeroed the ambient term entirely. AO off restores the
// full-strength flat ambient: byte-identical to the raster path.
fn rt_or_flat_ambient(albedo_rgb: vec3<f32>, frag_xy: vec2<f32>) -> vec3<f32> {
    if u.scene_params.w > 0.5 && u.fog_params.w > 0.5 {
        let irr = textureLoad(rt_irradiance_mask, vec2<i32>(frag_xy), 0);
        return albedo_rgb * u.scene_params.y * u.ambient_tint.rgb * AMBIENT_IRRADIANCE_SCALE * irr.a;
    }
    return albedo_rgb * u.scene_params.y * u.ambient_tint.rgb;
}

// Caster `slot`'s map coordinates `suv` ([0,1] over its own resolution)
// scaled into its square of the layer, clamped half a texel inside so
// filter taps never reach past the rendered square.
fn shadow_layer_uv(slot: i32, suv: vec2<f32>) -> vec2<f32> {
    let texel = casters[slot].shadow.z;
    let half_texel = vec2<f32>(0.5 * texel);
    let scale = 1.0 / (texel * f32(textureDimensions(shadow_maps).x));
    return clamp(suv, half_texel, vec2<f32>(1.0) - half_texel) * scale;
}

// One hardware comparison sample of caster `slot`'s shadow map.
// `textureSampleCompareLevel` (level 0, no derivatives) is legal in the
// data-dependent control flow the per-light loop creates — plain
// `textureSampleCompare` would not be.
fn sample_shadow(slot: i32, suv: vec2<f32>, ref_depth: f32) -> f32 {
    return textureSampleCompareLevel(shadow_maps, shadow_sampler, shadow_layer_uv(slot, suv), slot, ref_depth);
}

// Plain (non-comparison) nearest-texel depth read for the PCSS blocker
// search (D12), no hardware PCF.
fn plain_shadow_depth(slot: i32, uv: vec2<f32>) -> f32 {
    let clamped = clamp(uv, vec2<f32>(0.0), vec2<f32>(0.999999));
    let px = vec2<i32>(clamped / casters[slot].shadow.z);
    return textureLoad(shadow_maps, px, slot, 0);
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

// D12 — PCSS contact-hardening penumbra (REALTIME_3D_DESIGN section 11).
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
// lights[i*4+1].w — negative means this light casts no shadow, so the point
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
    // RAYTRACING_DESIGN.md RT-D3: single uniform-gated bool branch —
    // RT shadow masks replace shadow maps when rt_enabled AND rt_shadows
    // toggle are both active (rt_flags.w > 0.5). When rt_shadows is off,
    // maps are rendered and the raster PCSS/PCF path below runs. Native-
    // res `frag_xy` (`@builtin(position)`, already pixel coordinates)
    // indexes the shadow-mask textures directly — they're already full-res
    // and depth-aware-upsampled, no filtering needed. Caster slot c is
    // channel c % 4 of layer c / 4.
    // RT toggle: when rt_shadows is off (`rt_flags.w <= 0.5`), fall through
    // to raster shadow maps even though RT is active for other terms. The
    // sv textures weren't written (shadow_spp=0 in kernel), so reading them
    // would deliver stale history — this gate is the read side.
    if u.scene_params.w > 0.5 && u.rt_flags.w > 0.5 {
        let ch = u32(slot_f + 0.5);
        // A caster past the visibility arrays' capacity (memory refused
        // to grow them) is unshadowed, never another caster's layer.
        if ch / 4u >= u32(textureNumLayers(rt_shadow_mask)) {
            return 1.0;
        }
        return textureLoad(rt_shadow_mask, vec2<i32>(frag_xy), i32(ch / 4u), 0)[ch % 4u];
    }
    let slot = i32(slot_f + 0.5);
    let vp = casters[slot].view_proj;
    let params = casters[slot].shadow;
    let bias = params.x;
    let khw_raw = params.y;
    let texel = params.z;
    let light_size = params.w;
    // Texel 0 marks a caster with no shadow-map layer: unshadowed.
    if texel <= 0.0 {
        return 1.0;
    }

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

// `KHR_materials_sheen`'s Charlie
// (velvet) NDF — the glTF spec's own reference distribution.
fn D_Charlie(sheen_roughness: f32, n_dot_h: f32) -> f32 {
    let alpha = max(sheen_roughness * sheen_roughness, 0.0001);
    let inv_alpha = 1.0 / alpha;
    let cos2h = n_dot_h * n_dot_h;
    let sin2h = max(1.0 - cos2h, 0.0000001);
    return (2.0 + inv_alpha) * pow(sin2h, inv_alpha * 0.5) / (2.0 * PI);
}

// Ashikhmin visibility term — the glTF spec's own cited "simpler
// alternative" to the full Charlie/Estevez fitted visibility (whose
// lambda-function form is a multi-constant curve fit not worth this
// shader's added complexity for a single additive lobe). Already includes
// the `4·NdotV·NdotL` normalization, same convention as the base
// microfacet `specular` term below.
fn V_Ashikhmin(n_dot_v: f32, n_dot_l: f32) -> f32 {
    return 1.0 / max(4.0 * (n_dot_l + n_dot_v - n_dot_l * n_dot_v), 0.0001);
}

// `KHR_materials_iridescence`'s
// thin-film interference — the glTF spec's own reference "Airy summation"
// algorithm (Belcour/Barla 2017, mirrored verbatim across every glTF
// engine that implements this extension: the Khronos glTF-Sample-Viewer's
// `iridescence.glsl`, Three.js, Babylon.js, Filament's docs appendix).
// Ported term-for-term from that reference, not re-derived — see the XYZ
// sensitivity-curve magic constants below, which are a fitted
// approximation of the CIE color-matching functions and are NOT safe to
// hand-simplify.
const XYZ_TO_REC709_0: vec3<f32> = vec3<f32>(3.2404542, -1.5371385, -0.4985314);
const XYZ_TO_REC709_1: vec3<f32> = vec3<f32>(-0.9692660, 1.8760108, 0.0415560);
const XYZ_TO_REC709_2: vec3<f32> = vec3<f32>(0.0556434, -0.2040259, 1.0572252);

fn iridescence_f0_to_ior(f0: vec3<f32>) -> vec3<f32> {
    let sqrt_f0 = sqrt(clamp(f0, vec3<f32>(0.0), vec3<f32>(0.9999)));
    return (vec3<f32>(1.0) + sqrt_f0) / (vec3<f32>(1.0) - sqrt_f0);
}

fn iridescence_ior_to_f0_v3(transmitted_ior: vec3<f32>, incident_ior: f32) -> vec3<f32> {
    let d = (transmitted_ior - vec3<f32>(incident_ior)) / (transmitted_ior + vec3<f32>(incident_ior));
    return d * d;
}

fn iridescence_ior_to_f0(transmitted_ior: f32, incident_ior: f32) -> f32 {
    let d = (transmitted_ior - incident_ior) / (transmitted_ior + incident_ior);
    return d * d;
}

fn iridescence_sensitivity(opd: f32, shift: vec3<f32>) -> vec3<f32> {
    let phase = 2.0 * PI * opd * 1.0e-9;
    let val = vec3<f32>(5.4856e-13, 4.4201e-13, 5.2481e-13);
    let pos = vec3<f32>(1.6810e+06, 1.7953e+06, 2.2084e+06);
    let var_ = vec3<f32>(4.3278e+09, 9.3046e+09, 6.6121e+09);

    var xyz = val * sqrt(2.0 * PI * var_) * cos(pos * phase + shift) * exp(-pow(phase, 2.0) * var_);
    xyz.x = xyz.x + 9.7470e-14 * sqrt(2.0 * PI * 4.5282e+09) * cos(2.2399e+06 * phase + shift.x) * exp(-4.5282e+09 * pow(phase, 2.0));
    xyz = xyz / 1.0685e-7;

    return vec3<f32>(dot(XYZ_TO_REC709_0, xyz), dot(XYZ_TO_REC709_1, xyz), dot(XYZ_TO_REC709_2, xyz));
}

fn schlick_fresnel(f0: vec3<f32>, f90: vec3<f32>, cos_theta: f32) -> vec3<f32> {
    let weight = pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
    return f0 + (f90 - f0) * weight;
}

// The split-sum LUT is parameterized by Schlick's F0. Convert measured view
// reflectance back to that parameter, keeping the grazing limit finite.
fn schlick_equivalent_f0(reflectance: vec3<f32>, f90: vec3<f32>, cos_theta: f32) -> vec3<f32> {
    let weight = pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
    let denominator = max(1.0 - weight, 0.0001);
    return clamp((reflectance - f90 * weight) / denominator, vec3<f32>(0.0), vec3<f32>(1.0));
}

// Returns the thin-film reflectance (RGB) to mix into the base F0 by
// `iridescenceFactor`. `outside_ior` is always 1.0 (air) in this shader —
// no nested-medium tracking. `base_f0` is the base layer's OWN F0 (the
// dielectric/metal Fresnel this doc's G-P4 phase already computes) — the
// thin film sits between air and that base layer.
fn eval_iridescence(outside_ior: f32, eta2: f32, cos_theta1: f32, thickness: f32, base_f0: vec3<f32>) -> vec3<f32> {
    // Force iridescence_ior -> outside_ior as thickness -> 0 (a vanishing
    // film has no optical effect) — smoothstep, not a hard branch, per the
    // reference.
    let t = smoothstep(0.0, 0.03, thickness);
    let iridescence_ior = mix(outside_ior, eta2, t);
    let sin_theta2_sq = (outside_ior / iridescence_ior) * (outside_ior / iridescence_ior) * (1.0 - cos_theta1 * cos_theta1);
    let cos_theta2_sq = 1.0 - sin_theta2_sq;
    if cos_theta2_sq < 0.0 {
        // Total internal reflection at the film's outer interface.
        return vec3<f32>(1.0);
    }
    let cos_theta2 = sqrt(cos_theta2_sq);

    // First interface (air -> film).
    let r0 = iridescence_ior_to_f0(iridescence_ior, outside_ior);
    let r12 = r0 + (1.0 - r0) * pow(clamp(1.0 - cos_theta1, 0.0, 1.0), 5.0);
    let t121 = 1.0 - r12;
    var phi12 = 0.0;
    if iridescence_ior < outside_ior {
        phi12 = PI;
    }
    let phi21 = PI - phi12;

    // Second interface (film -> base layer).
    let base_ior = iridescence_f0_to_ior(clamp(base_f0, vec3<f32>(0.0), vec3<f32>(0.9999)));
    let r1 = iridescence_ior_to_f0_v3(base_ior, iridescence_ior);
    let r23 = r1 + (vec3<f32>(1.0) - r1) * pow(clamp(1.0 - cos_theta2, 0.0, 1.0), 5.0);
    var phi23 = vec3<f32>(0.0);
    if base_ior.x < iridescence_ior { phi23.x = PI; }
    if base_ior.y < iridescence_ior { phi23.y = PI; }
    if base_ior.z < iridescence_ior { phi23.z = PI; }

    // Phase shift + optical path difference.
    let opd = 2.0 * iridescence_ior * thickness * cos_theta2;
    let phi = vec3<f32>(phi21) + phi23;

    // Compound terms — the Airy summation's geometric series.
    let r123 = clamp(vec3<f32>(r12) * r23, vec3<f32>(1e-5), vec3<f32>(0.9999));
    let sqrt_r123 = sqrt(r123);
    let rs = (t121 * t121) * r23 / (vec3<f32>(1.0) - r123);

    var i = vec3<f32>(r12) + rs;
    var cm = rs - vec3<f32>(t121);
    for (var m = 1; m <= 2; m = m + 1) {
        cm = cm * sqrt_r123;
        let sm = 2.0 * iridescence_sensitivity(f32(m) * opd, f32(m) * phi);
        i = i + cm * sm;
    }
    return max(i, vec3<f32>(0.0));
}

// KHR_materials_clearcoat's committed constant: "the clearcoat employs a
// fixed F0 of 0.04, corresponding to an IOR of 1.5".
const CLEARCOAT_F0: f32 = 0.04;

// One resolved PBR surface point. The caller resolves it (a mesh fragment
// from its material maps, a full-screen pass from its own surface); the
// light loop and the image-based lighting below read nothing else.
struct PbrSurface {
    world_pos: vec3<f32>,
    // `@builtin(position).xy`: indexes the full-res RT textures and seeds
    // the PCSS noise.
    frag_xy: vec2<f32>,
    // Toward the camera.
    view: vec3<f32>,
    // Shading normal, turned to face the view.
    normal: vec3<f32>,
    // Clearcoat's own shading normal (the geometric normal when unmapped).
    coat_normal: vec3<f32>,
    albedo: vec3<f32>,
    metallic: f32,
    roughness: f32,
    base_f0: vec3<f32>,
    base_f90: vec3<f32>,
    // KHR_materials_iridescence: (factor, film ior, film thickness in nm).
    iridescence: vec3<f32>,
    // KHR_materials_anisotropy: strength in [0, 1] and the rotated
    // tangent/bitangent the lobe stretches along.
    anisotropy_strength: f32,
    aniso_t: vec3<f32>,
    aniso_b: vec3<f32>,
    // KHR_materials_clearcoat: zero means no coat.
    clearcoat: f32,
    clearcoat_roughness: f32,
    // KHR_materials_sheen: a zero colour means no sheen.
    sheen_color: vec3<f32>,
    sheen_roughness: f32,
    // KHR_materials_diffuse_transmission: rgb colour, a factor.
    translucency: vec4<f32>,
}

// Light visibility per channel: the shadow factor, except the designated
// RT sun caster, whose visibility is its traced rgb transmission tint
// (RAYTRACING_DESIGN.md section 16 TL5: a red petal between sun and floor
// pools red). One textureLoad (I-TL3).
fn light_visibility(caster_slot: f32, world_pos: vec3<f32>, frag_xy: vec2<f32>) -> vec3<f32> {
    let vis = shadow_factor(world_pos, caster_slot, frag_xy);
    var vis3 = vec3<f32>(vis);
    if u.rt_flags.z > 0.5 && i32(caster_slot + 0.5) == i32(u.rt_flags.z - 0.5) {
        vis3 = textureLoad(rt_sun_tint, vec2<i32>(frag_xy), 0).rgb;
    }
    return vis3;
}

// Every light's direct term, split by lobe so the caller can compose them.
struct PbrDirect {
    // Diffuse plus base specular.
    total: vec3<f32>,
    // The diffuse part of `total` alone (transmission replaces it).
    diffuse: vec3<f32>,
    coat: vec3<f32>,
    sheen: vec3<f32>,
    // Diffuse transmission from lights behind the surface.
    translucent: vec3<f32>,
}

// Cook-Torrance GGX specular plus Lambert diffuse, summed over every wired
// light, with the clearcoat, sheen and diffuse transmission lobes beside it.
// The lobes accumulate apart and never feed `total`, so a material without
// them gets exactly the base result.
fn pbr_direct_lighting(s: PbrSurface) -> PbrDirect {
    let N = s.normal;
    let V = s.view;
    let Nc = s.coat_normal;
    let world_pos = s.world_pos;
    let frag_xy = s.frag_xy;
    let albedo = s.albedo;
    let metallic = s.metallic;
    let base_f0 = s.base_f0;
    let base_f90 = s.base_f90;
    let iridescence = s.iridescence;
    let iridescence_factor = iridescence.x;
    let anisotropy_strength = s.anisotropy_strength;
    let aniso_t = s.aniso_t;
    let aniso_b = s.aniso_b;
    let clearcoat = s.clearcoat;
    let sheen_color = s.sheen_color;
    let sheen_roughness = s.sheen_roughness;
    let translucency = s.translucency;

    let n_dot_v = max(dot(N, V), 0.001);
    let a = s.roughness * s.roughness;
    let a2 = a * a;
    // glTF KHR_materials_anisotropy: alphaT widens toward 1 with the square
    // of strength while alphaB remains the authored alpha. At zero strength
    // both widths are exactly alpha, preserving isotropic continuity.
    var at = a;
    let ab = a;
    if anisotropy_strength > 0.0 {
        at = mix(a, 1.0, anisotropy_strength * anisotropy_strength);
    }
    // Clearcoat: the same GGX shape at its own roughness, Schlick-GGX k.
    // Only read when the coat is present.
    let cc_n_dot_v = max(dot(Nc, V), 0.001);
    var cc_a2 = 0.00000001;
    var cc_k = 0.125;
    var cc_g_v = 1.0;
    if clearcoat > 0.0 {
        let cc_a = s.clearcoat_roughness * s.clearcoat_roughness;
        cc_a2 = cc_a * cc_a;
        let cc_r = s.clearcoat_roughness + 1.0;
        cc_k = (cc_r * cc_r) / 8.0;
        cc_g_v = cc_n_dot_v / (cc_n_dot_v * (1.0 - cc_k) + cc_k);
    }

    let has_sheen = any(sheen_color > vec3<f32>(0.0));
    var direct = vec3<f32>(0.0);
    var direct_coat = vec3<f32>(0.0);
    // Accumulated in parallel with `direct` from the same already-computed
    // terms, never substituted into it, so it cannot perturb `direct`.
    var direct_diffuse = vec3<f32>(0.0);
    var direct_sheen = vec3<f32>(0.0);
    var direct_translucent = vec3<f32>(0.0);
    let light_count = u32(u.scene_params.x);
    for (var i = 0u; i < light_count; i = i + 1u) {
        let l_dir = light_direction_attenuation(i, world_pos);
        let l_col = lights[i * LIGHT_STRIDE + 1u];
        let L = l_dir.xyz;
        let half_length = length(L + V);
        let H = (L + V) / max(half_length, 1e-6);
        let n_dot_l = max(dot(N, L), 0.0);
        let n_dot_h = max(dot(N, H), 0.0);
        let v_dot_h = max(dot(V, H), 0.001);

        let nh2 = clamp(n_dot_h * n_dot_h, 0.0, 1.0);
        let denom_d = (1.0 - nh2) + nh2 * a2;
        let D = a2 / (PI * denom_d * denom_d);

        // Exact isotropic limit of the anisotropic height-correlated Smith
        // visibility below. A different zero-strength approximation creates
        // a visible jump as soon as an anisotropy map becomes nonzero.
        let nv2 = min(n_dot_v * n_dot_v, 1.0);
        let nl2 = min(n_dot_l * n_dot_l, 1.0);
        let lambda_v_iso = n_dot_l * sqrt(a2 * (1.0 - nv2) + nv2);
        let lambda_l_iso = n_dot_v * sqrt(a2 * (1.0 - nl2) + nl2);
        let visibility = 0.5 / max(lambda_v_iso + lambda_l_iso, 0.0000001);

        let plain_f = schlick_fresnel(base_f0, base_f90, v_dot_h);
        var F = plain_f;
        if iridescence_factor > 0.0 {
            let film_f = eval_iridescence(1.0, iridescence.y, v_dot_h, iridescence.z, base_f0);
            F = mix(plain_f, film_f, iridescence_factor);
        }
        // Only the directional widths differ from the isotropic limit.
        var specular = vec3<f32>(0.0);
        // Reflection exists only in the positive hemispheres and requires a
        // nonzero half-vector. Keep backlit diffuse transmission outside.
        if n_dot_l > 0.0 && dot(N, V) > 0.0 && half_length > 1e-6 {
            specular = D * visibility * F;
            if anisotropy_strength > 0.0 {
                let t_dot_h = dot(aniso_t, H);
                let b_dot_h = dot(aniso_b, H);
                // Divide by the widths before squaring. This equivalent GGX
                // form stays well-scaled at the peak without clipping narrow lobes.
                let scaled_h = vec3<f32>(t_dot_h / at, b_dot_h / ab, n_dot_h);
                let d_denom = dot(scaled_h, scaled_h);
                let d_aniso = 1.0 / (PI * at * ab * d_denom * d_denom);
                let t_dot_v = dot(aniso_t, V);
                let b_dot_v = dot(aniso_b, V);
                let t_dot_l = dot(aniso_t, L);
                let b_dot_l = dot(aniso_b, L);
                let lambda_v = n_dot_l * length(vec3<f32>(at * t_dot_v, ab * b_dot_v, n_dot_v));
                let lambda_l = n_dot_v * length(vec3<f32>(at * t_dot_l, ab * b_dot_l, n_dot_l));
                let vis_aniso = 0.5 / max(lambda_v + lambda_l, 0.0000001);
                specular = vec3<f32>(d_aniso * vis_aniso) * F;
            }
        }
        let f_max = max(F.r, max(F.g, F.b));
        let kd = (1.0 - f_max) * (1.0 - metallic);
        let diffuse = kd * albedo.rgb / PI;

        let vis3 = light_visibility(l_col.w, world_pos, frag_xy);
        direct = direct + (diffuse + specular) * l_col.rgb * n_dot_l * l_dir.w * vis3;
        direct_diffuse = direct_diffuse + diffuse * l_col.rgb * n_dot_l * l_dir.w * vis3;

        // Coat lobe: same D_GGX/G_Smith shape at clearcoat_roughness,
        // fixed F0 = 0.04, no diffuse term (a clear lacquer has none). `Nc`
        // equals `N` without a clearcoat normal map, so the coat terms then
        // match the base ones exactly.
        if clearcoat > 0.0 && dot(Nc, L) > 0.0 && dot(Nc, V) > 0.0 && half_length > 1e-6 {
            let cc_n_dot_l = max(dot(Nc, L), 0.0);
            let cc_n_dot_h = max(dot(Nc, H), 0.0);
            let cc_nh2 = clamp(cc_n_dot_h * cc_n_dot_h, 0.0, 1.0);
            let denom_d_coat = (1.0 - cc_nh2) + cc_nh2 * cc_a2;
            let D_coat = cc_a2 / (PI * denom_d_coat * denom_d_coat);
            let g_l_coat = cc_n_dot_l / (cc_n_dot_l * (1.0 - cc_k) + cc_k);
            let G_coat = cc_g_v * g_l_coat;
            let F_coat = CLEARCOAT_F0 + (1.0 - CLEARCOAT_F0) * pow(clamp(1.0 - v_dot_h, 0.0, 1.0), 5.0);
            let specular_coat = (D_coat * G_coat * F_coat) / (4.0 * cc_n_dot_v * cc_n_dot_l + 0.0001);
            direct_coat = direct_coat + specular_coat * l_col.rgb * cc_n_dot_l * l_dir.w * vis3;
        }

        // Charlie NDF times Ashikhmin visibility, tinted by sheenColor: no
        // Fresnel term (the sheen lobe is grazing-independent), no diffuse
        // term (a fabric fuzz has none of its own).
        if has_sheen {
            let D_sheen = D_Charlie(sheen_roughness, n_dot_h);
            let V_sheen = V_Ashikhmin(n_dot_v, n_dot_l);
            let specular_sheen = sheen_color * D_sheen * V_sheen;
            direct_sheen = direct_sheen + specular_sheen * l_col.rgb * n_dot_l * l_dir.w * vis3;
        }
        if translucency.a > 0.0 {
            let back_l = max(dot(-N, L), 0.0);
            direct_translucent += translucency.a * kd * translucency.rgb / PI
                * l_col.rgb * back_l * l_dir.w * vis3;
        }
    }
    return PbrDirect(direct, direct_diffuse, direct_coat, direct_sheen, direct_translucent);
}

// Prefiltered environment radiance along a reflection, roughness picking the
// mip. RAYTRACING_DESIGN.md section 9 RD1: when RT reflections ran this
// frame, the traced radiance substitutes for the fetch (never adds: the same
// quantity), but only where the kernel wrote one (`.a >= 0`, BUG-88m:
// Blend fragments and Mask holes keep the environment).
fn ibl_reflection(r_uv: vec2<f32>, roughness: f32, frag_xy: vec2<f32>) -> vec3<f32> {
    var prefiltered = textureSampleLevel(prefiltered_specular, envmap_sampler, r_uv, roughness * PREFILTER_MAX_MIP).rgb;
    if u.scene_params.w > 0.5 && u.rt_flags.x > 0.5 {
        let rt_refl = textureLoad(rt_reflection, vec2<i32>(frag_xy), 0);
        if rt_refl.a >= 0.0 {
            prefiltered = rt_refl.rgb;
        }
    }
    return prefiltered;
}

// Cosine-convolved diffuse irradiance at a normal. RAYTRACING_DESIGN.md
// section 14 ED2/ED6: when the GI gather ran this frame (`rt_flags.y`), the
// traced env+GI irradiance substitutes for the map (same quantity, same
// scale). Background texels hold the kernel's void value (rgb 0, ao 1);
// there the map stands, so a transparent surface over empty sky never goes
// black.
fn ibl_irradiance(normal: vec3<f32>, frag_xy: vec2<f32>) -> vec3<f32> {
    let n_uv = env_lookup_uv(normal);
    var irradiance = textureSampleLevel(irradiance_map, envmap_sampler, n_uv, 0.0).rgb;
    if u.scene_params.w > 0.5 && u.rt_flags.y > 0.5 {
        let traced = textureLoad(rt_irradiance_mask, vec2<i32>(frag_xy), 0);
        if !all(traced.rgb == vec3<f32>(0.0)) || traced.a < 1.0 {
            irradiance = traced.rgb;
        }
    }
    return irradiance;
}

// The image-based lighting of one surface point.
struct PbrIbl {
    // Base specular reflection.
    specular: vec3<f32>,
    coat: vec3<f32>,
    sheen: vec3<f32>,
    // One minus the sheen lobe's directional albedo: scales the layers
    // under sheen so it adds no energy.
    sheen_base_scale: f32,
    // Diffuse irradiance at the shading normal.
    irradiance: vec3<f32>,
    // Split-sum LUT (scale, bias) at (n·v, roughness).
    env_brdf: vec2<f32>,
    // View reflectance and its Schlick-equivalent F0 for the LUT.
    f_view: vec3<f32>,
    ibl_f0: vec3<f32>,
}

// Split-sum IBL (IMPORT_FIDELITY_DESIGN.md D2/F-P1): prefiltered specular
// with the BRDF LUT's scale and bias, plus cosine-convolved irradiance.
// Fresnel uses N·V, the standard split-sum substitute and the only choice
// that is defined with zero lights.
fn pbr_ibl(s: PbrSurface) -> PbrIbl {
    let N = s.normal;
    let V = s.view;
    let Nc = s.coat_normal;
    let roughness = s.roughness;
    let base_f0 = s.base_f0;
    let base_f90 = s.base_f90;
    let iridescence = s.iridescence;
    let iridescence_factor = iridescence.x;
    let anisotropy_strength = s.anisotropy_strength;
    let aniso_b = s.aniso_b;
    let clearcoat_roughness = s.clearcoat_roughness;
    let sheen_color = s.sheen_color;
    let sheen_roughness = s.sheen_roughness;
    let n_dot_v = max(dot(N, V), 0.001);
    let cc_n_dot_v = max(dot(Nc, V), 0.001);

    let R = reflect(-V, N);
    let r_uv = env_lookup_uv(R);
    let prefiltered = ibl_reflection(r_uv, roughness, s.frag_xy);
    let irradiance = ibl_irradiance(N, s.frag_xy);

    let env_brdf = textureSampleLevel(brdf_lut, opaque_scene_color_sampler, vec2<f32>(n_dot_v, roughness), 0.0).rg;
    let view_plain_f = schlick_fresnel(base_f0, base_f90, n_dot_v);
    var f_view = view_plain_f;
    if iridescence_factor > 0.0 {
        let view_film_f = eval_iridescence(1.0, iridescence.y, n_dot_v, iridescence.z, base_f0);
        f_view = mix(view_plain_f, view_film_f, iridescence_factor);
    }
    let ibl_f0 = schlick_equivalent_f0(f_view, base_f90, n_dot_v);
    var specular_ibl = prefiltered * (ibl_f0 * env_brdf.x + base_f90 * env_brdf.y);
    // Anisotropic IBL by the "bent normal" trick (Filament, Kaplanyan 2016):
    // bend the reflection toward the anisotropic tangent's cross-normal
    // before the lookup. A branch, not an unconditional `mix`, because
    // `0.0 * NaN` is NaN: a degenerate grazing `normalize(cross(...))` must
    // not reach isotropic materials. Clearcoat keeps the unbent reflection.
    if anisotropy_strength > 0.0 {
        let aniso_bend_tangent = cross(aniso_b, V);
        let bend = cross(aniso_bend_tangent, aniso_b);
        let bend_normal = select(N, bend / max(length(bend), 1e-6), dot(bend, bend) > 1e-12);
        let aniso_bent_normal = normalize(mix(N, bend_normal, anisotropy_strength));
        let r_aniso = reflect(-V, aniso_bent_normal);
        let r_aniso_uv = env_lookup_uv(r_aniso);
        let prefiltered_aniso = textureSampleLevel(prefiltered_specular, envmap_sampler, r_aniso_uv, roughness * PREFILTER_MAX_MIP).rgb;
        // RAYTRACING_DESIGN.md section 9 RD5: with RT reflections on, the
        // one traced value above stands here too (I-R3).
        var aniso_spec = prefiltered_aniso;
        if u.scene_params.w > 0.5 && u.rt_flags.x > 0.5 {
            aniso_spec = prefiltered;
        }
        specular_ibl = aniso_spec * (ibl_f0 * env_brdf.x + base_f90 * env_brdf.y);
    }

    // Coat IBL: the same split-sum resample at clearcoat_roughness on `Nc`.
    var coat_ibl = vec3<f32>(0.0);
    if s.clearcoat > 0.0 {
        let r_coat = reflect(-V, Nc);
        let r_coat_uv = env_lookup_uv(r_coat);
        let coat_prefiltered = textureSampleLevel(prefiltered_specular, envmap_sampler, r_coat_uv, clearcoat_roughness * PREFILTER_MAX_MIP).rgb;
        let coat_env_brdf = textureSampleLevel(brdf_lut, opaque_scene_color_sampler, vec2<f32>(cc_n_dot_v, clearcoat_roughness), 0.0).rg;
        coat_ibl = coat_prefiltered * (CLEARCOAT_F0 * coat_env_brdf.x + coat_env_brdf.y);
    }

    // Charlie directional albedo is integrated independently of GGX in
    // LUT.b. Its environment chain uses the same Charlie distribution.
    var sheen_ibl = vec3<f32>(0.0);
    var sheen_base_scale = 1.0;
    if any(sheen_color > vec3<f32>(0.0)) {
        let sheen_energy = clamp(textureSampleLevel(brdf_lut, opaque_scene_color_sampler,
            vec2<f32>(n_dot_v, sheen_roughness), 0.0).b, 0.0, 1.0);
        let sheen_env = textureSampleLevel(prefiltered_sheen, envmap_sampler,
            r_uv, sheen_roughness * PREFILTER_MAX_MIP).rgb;
        sheen_ibl = sheen_color * sheen_env * sheen_energy;
        sheen_base_scale = 1.0 - max(sheen_color.r, max(sheen_color.g, sheen_color.b)) * sheen_energy;
    }

    return PbrIbl(specular_ibl, coat_ibl, sheen_ibl, sheen_base_scale, irradiance, env_brdf, f_view, ibl_f0);
}

// KHR_materials_transmission +
// KHR_materials_volume screen-space refraction — the real glass pass that
// REPLACES the D8/F-P5 alpha-blend approximation (D3 explicitly rejects
// keeping that approximation once real refraction ships; see fs_pbr's call
// site for how the two are kept from double-applying). Follows the Khronos
// glTF-Sample-Viewer reference shape (getVolumeTransmissionRay /
// getIBLVolumeRefraction, the house-standard real-time approximation, not
// ray tracing): refract the view ray through the surface, walk
// `thickness_factor` world units along it (scaled by the object's own
// model-matrix scale, so a non-uniformly-scaled mesh's glass reads the
// right thickness), project the exit point back into `opaque_scene_color`'s
// screen space, and Beer-Lambert-tint the sample. Roughness blurs the
// sample via `opaque_scene_color`'s own mip chain (E2b, F-P6 mip-gen
// infra), the SAME roughness*max_mip shape `prefiltered_specular` already
// uses above (`roughness * PREFILTER_MAX_MIP`) — not a new formula.
// `transmission_factor == 0.0` short-circuits before this ever runs (see
// call site) — zero cost, zero shading change for every non-glass object.
//
// Refract the view ray (I = -V, the direction light travels INTO the
// surface) through the dielectric interface at the given `ior`, walk
// `thickness` world units along it, and sample `opaque_scene_color` at the
// exit point's screen-space projection. Factored out of `transmission_diffuse`
// so E6's dispersion can call it once per channel with a different `ior`
// each time (see below) — a single call at the base `ior` reproduces
// pre-E6 `transmission_diffuse`'s inline computation exactly.
fn sample_transmission(
    N: vec3<f32>,
    V: vec3<f32>,
    world_pos: vec3<f32>,
    thickness: f32,
    model_scale: vec3<f32>,
    ior: f32,
    mip_level: f32,
) -> vec3<f32> {
    // WGSL's `refract` returns a zero vector on total internal reflection
    // (grazing exits at high IOR) — fall back to straight-through along -N
    // so the sample never NaNs on normalize(0).
    var refraction_dir = refract(-V, N, 1.0 / max(ior, 1.0));
    if dot(refraction_dir, refraction_dir) < 1e-8 {
        refraction_dir = -N;
    }
    refraction_dir = normalize(refraction_dir);
    let transmission_ray = refraction_dir * thickness * model_scale;
    let exit_pos = world_pos + transmission_ray;
    let ndc = u.view_proj * vec4<f32>(exit_pos, 1.0);
    let ndc_xy = ndc.xy / max(ndc.w, 1e-6);
    // Same NDC->UV convention as `shadow_factor`'s `project_to_shadow_uv`
    // above: y flipped (Metal texture origin is top-left).
    let refraction_uv = vec2<f32>(ndc_xy.x * 0.5 + 0.5, ndc_xy.y * -0.5 + 0.5);
    let env_uv = env_lookup_uv(refraction_dir);
    let env = textureSampleLevel(prefiltered_specular, envmap_sampler, env_uv, mip_level).rgb;
    let edge = min(min(refraction_uv.x, refraction_uv.y), min(1.0 - refraction_uv.x, 1.0 - refraction_uv.y));
    let screen_weight = smoothstep(0.0, 0.025, edge) * select(0.0, 1.0, ndc.w > 0.0);
    let background = textureSampleLevel(opaque_scene_color, opaque_scene_color_sampler,
        clamp(refraction_uv, vec2<f32>(0.0), vec2<f32>(1.0)), mip_level);
    // The screen snapshot is premultiplied, exposed and fogged. Recover
    // radiance at this surface before its own fog/exposure is applied.
    // Empty background coverage receives the environment, not black.
    let screen_rgb = background.rgb * exp2(-u.scene_params.z);
    let fog_transmittance = exp(-max(u.fog_params.x, 0.0) * length(u.camera_pos.xyz - world_pos)
        * exp(-u.fog_params.y * max(world_pos.y, 0.0)));
    let surface_rgb = max((screen_rgb - u.fog_color.rgb * (1.0 - fog_transmittance) * background.a)
        / max(fog_transmittance, 1e-4), vec3<f32>(0.0));
    return mix(env, surface_rgb + env * (1.0 - background.a), screen_weight);
}

// Beer-Lambert absorption per metre of the volume material:
// -ln(attenuationColor) / attenuationDistance.
fn volume_attenuation_coefficient() -> vec3<f32> {
    let attenuation_distance = u.transmission_volume_params.z;
    let attenuation_color = u.volume_attenuation_color.rgb;
    let safe_color = max(attenuation_color, vec3<f32>(1e-6));
    return -log(safe_color) / max(attenuation_distance, 1e-4);
}

// Mip of `opaque_scene_color` a transmission sample reads: roughness scales
// the mip chain, fading in with the ior above 1.
fn transmission_mip_level(roughness: f32, ior: f32) -> f32 {
    let max_mip = f32(textureNumLevels(opaque_scene_color) - 1u);
    return clamp(roughness * clamp(ior * 2.0 - 2.0, 0.0, 1.0) * max_mip, 0.0, max_mip);
}

// The specular-reflectance fraction the split-sum IBL computes
// (F0*brdf.x + F90*brdf.y); the diffuse response under it is what remains.
fn transmission_specular_fraction(F0: vec3<f32>, F90: vec3<f32>, env_brdf: vec2<f32>) -> vec3<f32> {
    return F0 * env_brdf.x + F90 * env_brdf.y;
}
