// node.render_scene internal pass (VOLUMETRIC_LIGHT_DESIGN.md D2, P2) —
// half-res single-scattering light-shaft march. Committed algorithm (§2 D2's
// block), implemented verbatim — no substitution. Sun lights only in P2;
// Point lights are P3 (this file's `shaft_lights` buffer is pre-filtered to
// Sun-mode lights by render_scene.rs before this kernel ever sees it).
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

struct Uniforms {
    camera_pos: vec4<f32>,   // xyz, near
    camera_right: vec4<f32>, // xyz, far
    camera_up: vec4<f32>,    // xyz, fov_y
    camera_fwd: vec4<f32>,   // xyz, aspect
    fog_shaft: vec4<f32>,    // fog_density, height_falloff, shaft_anisotropy(g), shaft_intensity
    misc: vec4<f32>,         // steps(as f32), light_count(as f32), exposure_ev, 0
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var half_depth: texture_2d<f32>;
// Sun-only light array, SAME 2-vec4-per-light packing as render_scene.wgsl's
// `lights` storage buffer: [i*2] = (dir toward light, .xyz, .w unused here),
// [i*2+1] = (premultiplied color.rgb, .w = this light's caster slot index,
// -1 = no shadow).
@group(0) @binding(2) var<storage, read> shaft_lights: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> casters: array<vec4<f32>>;
@group(0) @binding(4) var shadow_map_0: texture_depth_2d;
@group(0) @binding(5) var shadow_map_1: texture_depth_2d;
@group(0) @binding(6) var shadow_map_2: texture_depth_2d;
@group(0) @binding(7) var shadow_map_3: texture_depth_2d;
@group(0) @binding(8) var shadow_sampler: sampler_comparison;
@group(0) @binding(9) var output_tex: texture_storage_2d<rgba16float, write>;

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
            let dir_to_light = shaft_lights[li * 2u];
            let color_slot = shaft_lights[li * 2u + 1u];
            let vis = shadow_vis(color_slot.w, x);
            // Sun attenuation = 1.0 (D2) — omitted, not multiplied by 1.
            let light_to_x_dir = -dir_to_light.xyz;
            let cos_theta = dot(ray_dir, light_to_x_dir);
            let phase = henyey_greenstein(g, cos_theta);
            accum = accum + transmittance * sigma * vis * phase * color_slot.rgb * seg;
        }
        transmittance = transmittance * exp(-sigma * seg);
    }

    // out = L * shaft_intensity * exp2(exposure_ev) (D2, exposed like the
    // scene, wgsl:543) — the composite pass adds this straight into color.
    let out_rgb = accum * shaft_intensity * exp2(exposure_ev);
    textureStore(output_tex, c, vec4<f32>(out_rgb, 0.0));
}
