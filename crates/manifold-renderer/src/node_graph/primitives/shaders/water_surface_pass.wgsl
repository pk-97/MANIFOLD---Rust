// node.render_scene water surface pass (WATER_SIMULATION_DESIGN.md section 7,
// S7) — a fullscreen depth-tested pass drawn after the opaque scene resolves:
// water fragments behind opaque depth are discarded by the depth test; visible
// fragments write the reconstructed water clip depth so later depth-aware
// effects see the water surface.
//
// V1 shading scope (the grey-lit integration checkpoint; shadow-receiving
// direct light is the named follow-up — see BUG-vglg):
//   Fresnel (Schlick, F0 from the material IOR) blends
//   - reflection: prefiltered IBL env sample at the material's roughness, plus
//     a direct-Sun GGX-ish specular lobe (lights arrive via the uniform, no
//     shadow maps yet), and
//   - transmission: the opaque-scene colour snapshot refracted by IOR with
//     Beer-Lambert attenuation exp(-sigma_a * thickness_eff).
// thickness_eff shortens the splat thickness to the first opaque hit; a
// displaced sample that would pull a foreground opaque object through the
// water is rejected (offset clamped to zero). Uncovered pixels discard.

struct WaterUniforms {
    inv_view: mat4x4<f32>,
    // view_z reconstruction + view-position unprojection
    near: f32,
    far: f32,
    tan_half_fov: f32,
    aspect: f32,
    // first Sun light, premultiplied colour; w = 1 when a Sun light exists
    sun_dir: vec4<f32>,   // direction TOWARD the light, w = sun present
    sun_color: vec4<f32>,
    // material
    ior: f32,
    roughness: f32,
    attenuation_distance: f32,
    thickness_scale: f32, // splat-thickness → metres calibration (S6 chord factor)
    attenuation_color: vec4<f32>,
    screen_dims: vec4<f32>, // w, h, 1/w, 1/h
}

@group(0) @binding(0) var<uniform> u: WaterUniforms;
@group(0) @binding(1) var water_depth: texture_2d<f32>;
@group(0) @binding(2) var water_thickness: texture_2d<f32>;
@group(0) @binding(3) var water_normals: texture_2d<f32>;
@group(0) @binding(4) var opaque_scene_color: texture_2d<f32>;
@group(0) @binding(5) var opaque_depth: texture_depth_2d;
@group(0) @binding(6) var prefiltered_specular: texture_2d<f32>;
@group(0) @binding(7) var env_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_full(@builtin(vertex_index) vi: u32) -> VsOut {
    var out: VsOut;
    let xy = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
    out.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return out;
}

fn view_z_of(raw: f32) -> f32 {
    // perspective_rh convention inverted: linear eye depth from raw [0,1] clip depth
    return u.near * u.far / (u.far - raw * (u.far - u.near));
}

fn view_pos_of(uv: vec2<f32>, raw: f32) -> vec3<f32> {
    let vz = view_z_of(raw);
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let vx = ndc.x * u.tan_half_fov * u.aspect * vz;
    let vy = ndc.y * u.tan_half_fov * vz;
    return vec3<f32>(vx, vy, -vz);
}

struct FsOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fs_water(in: VsOut) -> FsOut {
    var out: FsOut;
    let coord = vec2<i32>(in.pos.xy);
    let nrm = textureLoad(water_normals, coord, 0);
    let raw = textureLoad(water_depth, coord, 0).r;
    let view_pos = view_pos_of(in.uv, raw);
    // Keep derivative evaluation finite and uniform even for uncovered pixels:
    // invalid/degenerate normals use the documented toward-camera fallback.
    let n_view_len_sq = dot(nrm.xyz, nrm.xyz);
    var n_view = vec3<f32>(0.0, 0.0, -1.0);
    if (n_view_len_sq > 1e-12 && n_view_len_sq < 3.0e38) {
        n_view = nrm.xyz / sqrt(n_view_len_sq);
    }
    let n_view_dx = dpdxFine(n_view);
    let n_view_dy = dpdyFine(n_view);
    let normal_variation = min(
        0.5 * (dot(n_view_dx, n_view_dx) + dot(n_view_dy, n_view_dy)),
        0.04);
    // Preserve the original roughness exactly for a constant-normal surface.
    var spec_roughness = u.roughness;
    if (normal_variation > 0.0) {
        spec_roughness = sqrt(min(u.roughness * u.roughness + normal_variation, 1.0));
    }
    if nrm.a < 0.5 {
        discard;
    }
    // Frame bridge: normals_from_depth emits view normals in the splat
    // frame (+z toward the camera — see splat_view_center's z negation),
    // while inv_view is right-handed (-z forward). Negating z once maps
    // between the frames; without it the surface normal points INTO the
    // scene, ndotv clamps to 0, fresnel saturates to 1 and the shading
    // collapses to the reflection term alone.
    let n = normalize((u.inv_view * vec4<f32>(n_view.x, n_view.y, -n_view.z, 0.0)).xyz);
    let v = normalize(-(u.inv_view * vec4<f32>(view_pos, 0.0)).xyz);

    // Shorten the optical thickness to the first opaque hit behind the water.
    let splat_thickness = textureLoad(water_thickness, coord, 0).r * u.thickness_scale;
    let opaque_raw = textureLoad(opaque_depth, coord, 0);
    let opaque_vz = view_z_of(opaque_raw);
    let water_vz = view_z_of(raw);
    // Both terms are metres along the view axis. Keep the gap non-negative
    // when the opaque depth is clear or numerically in front of the splat.
    let view_ray_z = max(abs(normalize(view_pos).z), 1e-4);
    let axial_gap = max(opaque_vz - water_vz, 0.0);
    let opaque_ray_length = axial_gap / view_ray_z;
    let thickness_eff = clamp(min(splat_thickness, opaque_ray_length), 0.0, 1e3);

    // Refraction: displace the opaque-scene sample along the refracted dir,
    // scaled by the effective thickness. Reject a displacement that lands
    // behind a FOREGROUND opaque surface (would pull it through the water).
    let eta = 1.0 / u.ior;
    var refr_uv = in.uv;
    // Refraction is evaluated in view space, then the displaced endpoint is
    // projected through the same perspective model used by view_pos_of.
    // Adding a world-space direction directly to UVs breaks with orbit/FOV/
    // aspect changes and makes the distortion camera-dependent.
    let refr_view_normal = normalize(vec3<f32>(n_view.x, n_view.y, -n_view.z));
    let refr_view = refract(-normalize(-view_pos), refr_view_normal, eta);
    if dot(refr_view, refr_view) > 0.5 {
        let endpoint = view_pos + refr_view * thickness_eff;
        let endpoint_z = max(-endpoint.z, 1e-4);
        let endpoint_ndc = vec2<f32>(
            endpoint.x / (u.tan_half_fov * u.aspect * endpoint_z),
            endpoint.y / (u.tan_half_fov * endpoint_z));
        let cand = clamp(vec2<f32>(endpoint_ndc.x * 0.5 + 0.5,
                                   0.5 - endpoint_ndc.y * 0.5),
                         vec2<f32>(0.0), vec2<f32>(1.0));
        let cand_coord = vec2<i32>(min(cand * u.screen_dims.xy,
                                       u.screen_dims.xy - vec2<f32>(1.0)));
        let cand_opaque_raw = textureLoad(opaque_depth, cand_coord, 0);
        if view_z_of(cand_opaque_raw) >= water_vz - 1e-3 {
            refr_uv = cand;
        }
    }
    let scene = textureSampleLevel(opaque_scene_color, env_sampler, refr_uv, u.roughness * 4.0).rgb;

    // Beer-Lambert: sigma_a derived from attenuation colour/distance.
    let sigma = -log(clamp(u.attenuation_color.rgb, vec3<f32>(1e-4), vec3<f32>(1.0))) / max(u.attenuation_distance, 1e-3);
    let transmit = exp(-sigma * thickness_eff);
    let transmitted = scene * transmit;

    // Reflection: prefiltered IBL at the material roughness (equirect UV,
    // same convention as the scene env sampling).
    let r = reflect(-v, n);
    let max_lod = 4.0;
    let env_uv = vec2<f32>(atan2(r.z, r.x) * 0.15915494 + 0.5, acos(clamp(r.y, -1.0, 1.0)) * 0.31830988);
    let env = textureSampleLevel(prefiltered_specular, env_sampler, env_uv, spec_roughness * max_lod).rgb;

    let ndotv = clamp(dot(n, v), 0.0, 1.0);
    let f0v = (u.ior - 1.0) / (u.ior + 1.0);
    let f0 = f0v * f0v;
    let fres = f0 + (1.0 - f0) * pow(1.0 - ndotv, 5.0);

    // Direct Sun: one specular lobe (Blinn-Phong mapped from roughness) —
    // water's base colour is the attenuated scene, not a Lambert term.
    // No shadow lookup in V1 (named follow-up).
    var sun = vec3<f32>(0.0);
    if u.sun_dir.w > 0.5 {
        let l = normalize(u.sun_dir.xyz);
        let h = normalize(l + v);
        let ndoth = max(dot(n, h), 0.0);
        let base_shininess = clamp(2.0 / max(u.roughness * u.roughness, 1e-3) - 2.0, 2.0, 1024.0);
        let filtered_shininess = clamp(2.0 / max(spec_roughness * spec_roughness, 1e-3) - 2.0, 2.0, 1024.0);
        // Normalize the existing fixed-peak Blinn approximation when filtering
        // broadens it, preserving its hemispherical integral. Flat normals keep
        // an exact scale of one; this is not a complete physical BRDF.
        var lobe_scale = 1.0;
        if (normal_variation > 0.0) {
            lobe_scale = (filtered_shininess + 1.0) / (base_shininess + 1.0);
        }
        sun = u.sun_color.rgb * lobe_scale * pow(ndoth, filtered_shininess) * max(dot(n, l), 0.0);
    }

    let col = mix(transmitted, env, fres) + sun;
    out.color = vec4<f32>(col, 1.0);
    out.depth = raw;
    return out;
}
