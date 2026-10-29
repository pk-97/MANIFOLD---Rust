// Full-screen shading of the water the march found. Reads the march's
// per-pixel `FieldHit`, lights it with the scene's own shading (the same
// functions meshes use) and lets the scene behind it through the way glass
// does. Writes the colour over the frame and the hit depth into the depth
// buffer later glass layers test against.

@group(0) @binding(60) var<storage, read> hits: array<FieldHit>;
@group(0) @binding(61) var<uniform> shade: ShadeUniforms;
// Per pixel (tau, m) from `water_mist.wgsl`: the mist's optical depth along the
// camera ray and its first moment about the ray origin. Read by `fs_mist` only.
@group(0) @binding(67) var<storage, read> mist_depth: array<vec2<f32>>;

struct ShadeUniforms {
    inv_view_proj: mat4x4<f32>,  // clip -> world, the same matrix the march uses
    target_size: vec4<f32>,      // x width, y height (x is the hits buffer's row length), zw 0
    slot: vec4<u32>,             // x = slot + 1 this draw shades, yzw 0
};

@vertex
fn vs_water(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(corners[vi], 0.0, 1.0);
}

struct WaterOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fs_water(@builtin(position) pos: vec4<f32>) -> WaterOut {
    let pix = vec2<u32>(pos.xy);
    let hit = hits[pix.y * u32(shade.target_size.x) + pix.x];
    if hit.depth <= 0.0 {
        discard;
    }
    let thickness_slot = unpack2x16float(hit.thickness_slot);
    if u32(round(thickness_slot.y)) != shade.slot.x {
        discard;
    }

    // World position from the depth, the march's convention.
    let uv = pos.xy / shade.target_size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let w = shade.inv_view_proj * vec4<f32>(ndc, hit.depth, 1.0);
    let world = w.xyz / w.w;
    let thickness = thickness_slot.x;
    var n = oct_decode(unpack2x16snorm(hit.normal_oct));
    let view = normalize(u.camera_pos.xyz - world);
    // The camera under the water sees the surface from behind.
    if dot(n, view) < 0.0 {
        n = -n;
    }
    n = look_ripple_detail(world, n, distance(u.camera_pos.xyz, world), foam_cover_at(pos.xy));

    // Water has no texture maps, so its reflectance comes from the uniforms
    // alone, as `fs_pbr` computes it for a material with none.
    let ior = u.pbr_metallic_roughness.z;
    let specular_factor = u.pbr_metallic_roughness.w;
    let base_f0 = min(pow((ior - 1.0) / (ior + 1.0), 2.0) * u.pbr_specular_tint.rgb, vec3<f32>(1.0))
        * specular_factor;
    let base_f90 = vec3<f32>(specular_factor);
    let surface = PbrSurface(
        world, pos.xy, view, n, n, u.base_color.rgb, 0.0, u.pbr_metallic_roughness.y,
        base_f0, base_f90,
        vec3<f32>(0.0),
        0.0, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0),
        0.0, 0.01,
        vec3<f32>(0.0), 0.0,
        vec4<f32>(0.0),
        vec4<f32>(1.0),
    );
    let direct = pbr_direct_lighting(surface);
    let ibl = pbr_ibl(surface);
    // No diffuse body: what is under the water comes through below.
    let reflection = direct.total - direct.diffuse + ibl.specular;

    // Light through the water as glass does it, with the march's thickness
    // as the path. Where nothing is drawn behind the water the layer stays
    // open: the water lets what is under the layer through, as it lets the
    // scene through, and adds only its own light. The environment lights the
    // water but is not seen through it, as the camera does not see it round
    // the water either.
    let behind = sample_transmission(
        n, view, world, thickness, vec3<f32>(1.0), ior,
        transmission_mip_level(surface.roughness, ior),
    );
    // The share of the light from behind that crosses the water.
    var carried = exp(-volume_attenuation_coefficient() * thickness);
    var through = behind.behind * carried;
    // The water's own scattering over the same path, where its material has any.
    let scattering = u.volume_optics.y;
    if scattering > 0.0 {
        through = volume_inscatter(through, scattering * thickness);
        carried = carried * exp(-scattering * thickness);
    }
    let passed = (vec3<f32>(1.0) - transmission_specular_fraction(ibl.ibl_f0, base_f90, ibl.env_brdf))
        * u.base_color.rgb;
    let transmitted = passed * through;

    let lit = look_thin_water_light(thickness, reflection + transmitted);
    let foamed = look_foam(world, pos.xy, view, n, lit);
    // Covered by the water and its foam, the rest open to what is under it.
    let open = dot(passed * carried, vec3<f32>(1.0 / 3.0)) * (1.0 - behind.coverage);
    let alpha = mix(1.0 - open, 1.0, foamed.a);
    // Fog covers only what the water covers, as the snapshot's own fog does.
    let fog = fog_amount(world);
    let rgb = (foamed.rgb * (1.0 - fog) + u.fog_color.rgb * fog * alpha) * exp2(u.scene_params.z);

    var out: WaterOut;
    out.color = vec4<f32>(rgb, alpha);
    out.depth = hit.depth;
    return out;
}

fn shade_unproject(ndc: vec2<f32>, raw_depth: f32) -> vec3<f32> {
    let w = shade.inv_view_proj * vec4<f32>(ndc, raw_depth, 1.0);
    return w.xyz / w.w;
}

// The mist over everything drawn so far, as premultiplied colour and coverage
// for the blend `dst * (1 - a) + colour`, which is `behind * T + L_in * (1 - T)`.
@fragment
fn fs_mist(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let pix = vec2<u32>(pos.xy);
    let depth = mist_depth[pix.y * u32(shade.target_size.x) + pix.x];
    let tau = depth.x;
    if tau <= 0.0 {
        discard;
    }
    let uv = pos.xy / shade.target_size.xy;
    let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
    let origin = shade_unproject(ndc, 1.0);
    let dir = normalize(shade_unproject(ndc, 0.5) - origin);
    let mean_pos = origin + dir * (depth.y / tau);
    let mist = look_mist(dir, mean_pos, pos.xy, tau);
    // The fog tints the mist's own light, which covers only `mist.a` of the
    // pixel: fogging the premultiplied colour would add the whole fog colour
    // wherever any trace of mist lies.
    return vec4<f32>(apply_fog(mist.rgb, mean_pos) * exp2(u.scene_params.z) * mist.a, mist.a);
}
