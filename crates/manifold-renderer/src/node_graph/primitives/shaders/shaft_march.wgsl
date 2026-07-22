// node.render_scene internal pass (VOLUMETRIC_LIGHT_DESIGN.md D2, P2/P3) —
// half-res single-scattering light-shaft march. Committed algorithm (§2 D2's
// block), implemented verbatim — no substitution. P3: every wired light
// (Sun AND Point) contributes; Point attenuation is D2's
// `1/(1+d²/range²)` (light.rs:261), and Point shadow sampling is
// frustum-clipped against the light's single-frustum view-proj (light.rs:
// 48-51) via the SAME `shadow_vis` caster-table lookup Sun already uses —
// outside the frustum (or no caster slot) falls through to `vis=1.0`
// unshadowed glow (D2's honest cost, not a bug).
// Internal to render_scene, not a graph atom (§2.5 audit).
//
// `linearize_depth` is forked (copied, not shared/concatenated) from
// `shared/depth_common.wgsl` — same "forked, not shared" convention every
// other hand-authored consumer in this codebase already uses
// (ssao_from_depth.wgsl, coc_from_depth.wgsl), so this file validates
// standalone under `tests/wgsl_validation.rs`'s auto-discovery. Both
// implementations MUST stay bit-for-bit the same formula (synthesis-drift
// is the bug class this convention risks — GBUFFER_DESIGN.md §2 D4).
fn linearize_depth(raw: f32, near: f32, far: f32) -> f32 {
    let range = far / (near - far);
    return (range * near) / (raw + range);
}

// Shadow lookup (`shadow_vis`) is forked from render_scene.wgsl's
// `project_to_shadow_uv`/`sample_shadow` — same caster-table layout
// (`CASTER_STRIDE` vec4s: view_proj columns 0-3, then (bias, kernel_half_
// width, texel_size, light_size)), ONE `textureSampleCompareLevel` tap, no
// PCF kernel (the half-res upsample is the softener, per D2). This file's
// header convention: forked, not shared — same discipline render_scene.wgsl
// itself documents for its own PCF loop.
//
// `linearize_depth` comes from depth_common.wgsl, string-concatenated ahead
// of this file at pipeline-creation time (render_scene.rs).

const PI: f32 = 3.14159265358979;
const CASTER_STRIDE: u32 = 5u;
// P3: 3 vec4s per light (was 2 in P2's Sun-only packing) — see the binding(2)
// doc comment below for the field layout.
const LIGHT_STRIDE: u32 = 3u;

struct Uniforms {
    camera_pos: vec4<f32>,   // xyz, near
    camera_right: vec4<f32>, // xyz, far
    camera_up: vec4<f32>,    // xyz, fov_y
    camera_fwd: vec4<f32>,   // xyz, aspect
    fog_shaft: vec4<f32>,    // fog_density, height_falloff, shaft_anisotropy(g), shaft_intensity
    // steps(as f32), light_count(as f32), exposure_ev, rt_enabled(as f32,
    // RAYTRACING_DESIGN.md §5.2 P3/D5 — was reserved/0 before P3).
    misc: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var half_depth: texture_2d<f32>;
// P3 (VOLUMETRIC_LIGHT_DESIGN.md's own P3): every wired light (Sun AND
// Point), 3 vec4s per light:
//   [i*3+0] = Sun: dir-toward-light (.xyz, toward the light, matches
//             `Light::light_dir_at`'s Sun case), .w = 0.0 (mode Sun)
//           = Point: light world position (.xyz), .w = 1.0 (mode Point)
//   [i*3+1] = premultiplied color.rgb, .w = this light's caster slot index
//             (-1 = no shadow, unshadowed glow per D2)
//   [i*3+2] = .x = attenuation range (Point only; ignored for Sun), rest 0
// RAYTRACING_DESIGN.md §5.2 P3 (D5, "emissive-colored volumetric glow"):
// `render_scene.rs` also appends one Point-mode entry per emissive object
// (world-space centroid as `pos`, the object's emission factor as `color`,
// slot -1 — unshadowed glow, same honest-cost convention as an unshadowed
// Point light) when RT is enabled — a real, physically-motivated light
// source in this SAME march, not a separate glow pass.
@group(0) @binding(2) var<storage, read> shaft_lights: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> casters: array<vec4<f32>>;
@group(0) @binding(4) var shadow_map_0: texture_depth_2d;
@group(0) @binding(5) var shadow_map_1: texture_depth_2d;
@group(0) @binding(6) var shadow_map_2: texture_depth_2d;
@group(0) @binding(7) var shadow_map_3: texture_depth_2d;
@group(0) @binding(8) var shadow_sampler: sampler_comparison;
@group(0) @binding(9) var output_tex: texture_storage_2d<rgba16float, write>;
// RAYTRACING_DESIGN.md §5.2 P3 (D5, "volumetric march sampling shadow-ray
// visibility instead of shadow-map lookups"): the SAME full-res RT
// sun-visibility mask the surface pass (RT-P1/P2's half-res dispatch +
// upsample) already computed — reused here for the Sun light's march
// visibility instead of a shadow-map lookup when `rt_enabled` (see
// `shadow_vis`'s call site below). Always bound (ABI-stub discipline: a
// 1x1 dummy when RT isn't active this frame, same texture render_scene.wgsl
// itself falls back to via `rt_mask_tex`) — reading it when `rt_enabled ==
// 0.0` never happens (the branch below gates on the flag first).
@group(0) @binding(10) var rt_shadow_mask: texture_2d<f32>;

fn sample_shadow(slot: i32, suv: vec2<f32>, ref_depth: f32) -> f32 {
    switch slot {
        case 0: { return textureSampleCompareLevel(shadow_map_0, shadow_sampler, suv, ref_depth); }
        case 1: { return textureSampleCompareLevel(shadow_map_1, shadow_sampler, suv, ref_depth); }
        case 2: { return textureSampleCompareLevel(shadow_map_2, shadow_sampler, suv, ref_depth); }
        default: { return textureSampleCompareLevel(shadow_map_3, shadow_sampler, suv, ref_depth); }
    }
}

// D2's `shadow(l, x)`: no caster slot (`slot_f < 0`) -> unshadowed glow
// (1.0); outside the caster's frustum -> also 1.0 (no shadow data there);
// otherwise ONE comparison tap, biased the same as the main pass.
fn shadow_vis(slot_f: f32, world_pos: vec3<f32>) -> f32 {
    if slot_f < 0.0 {
        return 1.0;
    }
    let slot = i32(slot_f + 0.5);
    let base = u32(slot) * CASTER_STRIDE;
    let vp = mat4x4<f32>(casters[base], casters[base + 1u], casters[base + 2u], casters[base + 3u]);
    let params = casters[base + 4u];
    let bias = params.x;

    let clip = vp * vec4<f32>(world_pos, 1.0);
    if clip.w <= 0.0 {
        return 1.0;
    }
    let ndc = clip.xyz / clip.w;
    let suv = vec2<f32>(ndc.x * 0.5 + 0.5, ndc.y * -0.5 + 0.5);
    if suv.x < 0.0 || suv.x > 1.0 || suv.y < 0.0 || suv.y > 1.0 || ndc.z > 1.0 || ndc.z < 0.0 {
        return 1.0;
    }
    return sample_shadow(slot, suv, ndc.z - bias);
}

// Henyey-Greenstein phase function, D2's exact formula.
fn henyey_greenstein(g: f32, cos_theta: f32) -> f32 {
    let g2 = g * g;
    let denom = pow(max(1.0 + g2 - 2.0 * g * cos_theta, 1e-6), 1.5);
    return (1.0 - g2) / (4.0 * PI * denom);
}

// CINEMATIC_POST_DESIGN.md D2's committed hash, reused verbatim as the
// march-start jitter (D2/D5: deterministic, no temporal accumulation).
fn hash01(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= u32(dims.x) || id.y >= u32(dims.y) {
        return;
    }
    let c = vec2<i32>(i32(id.x), i32(id.y));
    let dims_f = vec2<f32>(dims);

    let near = u.camera_pos.w;
    let far = u.camera_right.w;
    let fov_y = u.camera_up.w;
    let aspect = u.camera_fwd.w;

    let raw_depth = textureLoad(half_depth, c, 0).r;
    let view_z = linearize_depth(raw_depth, near, far);

    let uv = (vec2<f32>(c) + vec2<f32>(0.5, 0.5)) / dims_f;
    let ndc_x = uv.x * 2.0 - 1.0;
    let ndc_y = 1.0 - uv.y * 2.0;
    let tan_half_fov = tan(fov_y * 0.5);

    let cam_pos = u.camera_pos.xyz;
    let right = u.camera_right.xyz;
    let up = u.camera_up.xyz;
    let fwd = u.camera_fwd.xyz;

    // ray = camera -> world position of this pixel's resolved depth (D2).
    // world_pos = cam_pos + fwd*view_z + right*(ndc_x*aspect*tan_half_fov*view_z)
    //           + up*(ndc_y*tan_half_fov*view_z); ray = world_pos - cam_pos.
    let ray = fwd * view_z
        + right * (ndc_x * aspect * tan_half_fov * view_z)
        + up * (ndc_y * tan_half_fov * view_z);
    let ray_length = length(ray);
    let ray_dir = select(fwd, ray / max(ray_length, 1e-6), ray_length > 1e-6);

    let steps = u32(u.misc.x + 0.5);
    let light_count = u32(u.misc.y + 0.5);
    let exposure_ev = u.misc.z;
    let rt_enabled = u.misc.w > 0.5;

    // RAYTRACING_DESIGN.md §5.2 P3 (D5): the Sun light's march visibility,
    // resolved ONCE per pixel from the RT sun-visibility mask instead of a
    // per-step, per-sample shadow-map lookup — reusing the RT visibility
    // the surface pass already computed rather than re-testing occlusion
    // at each march sample's world position `x`. This IS an approximation
    // (the mask encodes the SURFACE hit's visibility, not this pixel's
    // in-between volume points) — acceptable for a directional light: real
    // sun occluders are large-scale geometry whose shadow boundary barely
    // shifts between the surface depth and the handful of march samples in
    // front of it, and it is the ONLY RT visibility data the shadow-ray
    // pass produces (RT-P1/P2 traced the Sun only, never per-arbitrary-
    // light volume rays). Point lights and the emissive pseudo-lights below
    // are unaffected — they keep the existing per-step `shadow_vis` lookup
    // (which already falls through to unshadowed glow at slot -1).
    var rt_sun_vis = 1.0;
    if rt_enabled {
        let full_dims = vec2<f32>(textureDimensions(rt_shadow_mask));
        let full_pix = vec2<i32>(min(uv * full_dims, full_dims - vec2<f32>(1.0, 1.0)));
        rt_sun_vis = textureLoad(rt_shadow_mask, full_pix, 0).r;
    }

    let fog_density = u.fog_shaft.x;
    let height_falloff = u.fog_shaft.y;
    let g = u.fog_shaft.z;
    let shaft_intensity = u.fog_shaft.w;

    let seg = ray_length / f32(steps);
    // Committed D2/D5 jitter: t0 = (hash(px) - 0.5) * seg, deterministic,
    // no temporal accumulation.
    let t0 = (hash01(vec2<f32>(c)) - 0.5) * seg;

    var transmittance = 1.0;
    var accum = vec3<f32>(0.0);
    for (var i: u32 = 0u; i < steps; i = i + 1u) {
        let t = seg * (f32(i) + 0.5) + t0;
        let x = cam_pos + ray_dir * t;
        let sigma = fog_density * exp(-height_falloff * max(x.y, 0.0));
        for (var li: u32 = 0u; li < light_count; li = li + 1u) {
            let base = li * LIGHT_STRIDE;
            let pos_or_dir = shaft_lights[base];
            let color_slot = shaft_lights[base + 1u];
            let range_v = shaft_lights[base + 2u];
            // RAYTRACING_DESIGN.md §5.2 P3/D5: the Sun entry (mode 0)
            // reuses the per-pixel RT visibility computed once above,
            // in place of `shadow_vis`'s shadow-map lookup, when RT is on.
            let is_sun = pos_or_dir.w < 0.5;
            let vis = select(shadow_vis(color_slot.w, x), rt_sun_vis, rt_enabled && is_sun);

            // D2: Sun att = 1.0, fixed L (dir toward light). Point att =
            // 1/(1+d²/range²) (light.rs:261), L = normalize(pos - x)
            // (recomputed per sample, matches `Light::light_dir_at`'s Point
            // case).
            var light_dir_toward_light: vec3<f32>;
            var att: f32;
            if pos_or_dir.w < 0.5 {
                light_dir_toward_light = pos_or_dir.xyz;
                att = 1.0;
            } else {
                let to_light = pos_or_dir.xyz - x;
                let d_sq = dot(to_light, to_light);
                let range = range_v.x;
                let r_sq = range * range;
                light_dir_toward_light = select(
                    vec3<f32>(0.0, 0.0, 1.0),
                    to_light * inverseSqrt(max(d_sq, 1e-12)),
                    d_sq > 1e-12,
                );
                att = select(1.0 / (1.0 + d_sq / max(r_sq, 1e-10)), 0.0, r_sq < 1e-10);
            }
            let light_to_x_dir = -light_dir_toward_light;
            let cos_theta = dot(ray_dir, light_to_x_dir);
            let phase = henyey_greenstein(g, cos_theta);
            accum = accum + transmittance * sigma * vis * att * phase * color_slot.rgb * seg;
        }
        transmittance = transmittance * exp(-sigma * seg);
    }

    // out = L * shaft_intensity * exp2(exposure_ev) (D2, exposed like the
    // scene, wgsl:543) — the composite pass adds this straight into color.
    let out_rgb = accum * shaft_intensity * exp2(exposure_ev);
    textureStore(output_tex, c, vec4<f32>(out_rgb, 0.0));
}
