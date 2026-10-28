// Look hook: surface foam. Called from the water shading with the shaded
// surface colour at the hit. Foam is a matte white scatterer lying on the
// water, covering a fraction `c` of the surface at the pixel (`foam_mask.wgsl`).
// Where it covers it hides the water's reflection and the light through it, so
// the colour is `mix(water, foam_lit, c)`, returned with `c` so the caller
// can count the foam as opaque. Foam also damps the small capillary
// ripples (surface films and foam damp capillary waves: Alpers and Huhnerfuss
// 1989, "The damping of ocean waves by surface films"), which
// `look_ripple_detail` applies by scaling its strength by `1 - c`.

// Foam that fully covers the water is a dense pack of bubbles that scatters
// nearly all visible light, diffusely, so it reads white like snow; 0.85
// keeps its shading. Thin foam's see-through rides the cover fraction `c`,
// not the albedo.
const FOAM_ALBEDO: f32 = 0.85;

fn look_foam(
    world_pos: vec3<f32>,
    frag_xy: vec2<f32>,
    view: vec3<f32>,
    normal: vec3<f32>,
    rgb: vec3<f32>,
) -> vec4<f32> {
    let c = foam_cover_at(frag_xy);
    if c <= 0.0 {
        return vec4<f32>(rgb, 0.0);
    }
    // A rough dielectric with no specular lobe: zero F0 and F90, every other
    // lobe off, as `fs_water` turns them off.
    let surface = PbrSurface(
        world_pos, frag_xy, view, normal, normal, vec3<f32>(FOAM_ALBEDO), 0.0, 1.0,
        vec3<f32>(0.0), vec3<f32>(0.0),
        vec3<f32>(0.0),
        0.0, vec3<f32>(1.0, 0.0, 0.0), vec3<f32>(0.0, 1.0, 0.0),
        0.0, 0.01,
        vec3<f32>(0.0), 0.0,
        vec4<f32>(0.0),
    );
    // `fs_pbr`'s own diffuse: the direct diffuse plus the irradiance times the
    // albedo (its diffuse energy split is one with no specular lobe).
    let foam_lit = pbr_direct_lighting(surface).diffuse + FOAM_ALBEDO * ibl_irradiance(normal, frag_xy);
    return vec4<f32>(mix(rgb, foam_lit, c), c);
}
