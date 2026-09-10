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
    if nrm.a < 0.5 {
        discard;
    }
    let raw = textureLoad(water_depth, coord, 0).r;
    let view_pos = view_pos_of(in.uv, raw);
    let n_view = normalize(nrm.xyz);
    let n = normalize((u.inv_view * vec4<f32>(n_view, 0.0)).xyz);
    let v = normalize(-(u.inv_view * vec4<f32>(view_pos, 0.0)).xyz);

    // Shorten the optical thickness to the first opaque hit behind the water.
    let splat_thickness = textureLoad(water_thickness, coord, 0).r * u.thickness_scale;
    let opaque_raw = textureLoad(opaque_depth, coord, 0);
    let opaque_vz = view_z_of(opaque_raw);
    let water_vz = view_z_of(raw);
    let thickness_eff = clamp(min(splat_thickness, opaque_vz - water_vz), 0.0, 1e3);

    // Refraction: displace the opaque-scene sample along the refracted dir,
    // scaled by the effective thickness. Reject a displacement that lands
    // behind a FOREGROUND opaque surface (would pull it through the water).
    let eta = 1.0 / u.ior;
    var refr_uv = in.uv;
    let refr = refract(-v, n, eta);
    if refr.x * refr.x + refr.y * refr.y + refr.z * refr.z > 0.5 {
        let off = vec2<f32>(refr.x, -refr.y) * thickness_eff * 0.5;
        let cand = clamp(in.uv + off, vec2<f32>(0.0), vec2<f32>(1.0));
        let cand_coord = vec2<i32>(cand * u.screen_dims.xy);
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
    let env = textureSampleLevel(prefiltered_specular, env_sampler, env_uv, u.roughness * max_lod).rgb;

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
        let shininess = 2.0 / max(u.roughness * u.roughness, 1e-3) - 2.0;
        sun = u.sun_color.rgb * pow(ndoth, clamp(shininess, 2.0, 1024.0)) * max(dot(n, l), 0.0);
    }

    let col = mix(transmitted, env, fres) + sun;
    out.color = vec4<f32>(col, 1.0);
    out.depth = raw;
    return out;
}
