// Look hook: surface foam. Called from the water shading with the shaded
// surface colour at the hit. Foam is a matte white scatterer lying on the
// water, covering a fraction `c` of each surface patch (`foam_field.wgsl`).
// Where it covers it hides the water's reflection and the light through it, so
// the colour is `mix(water, foam_lit, c)`. Foam also damps the small capillary
// ripples (surface films and foam damp capillary waves: Alpers and Huhnerfuss
// 1989, "The damping of ocean waves by surface films"), which
// `look_ripple_detail` applies by scaling its strength by `1 - c`.

// Thick fresh sea foam reflects about 55% of visible light, diffusely
// (Whitlock, Bartlett and Gurganus 1982, "Sea foam reflectance and influence on
// optimum wavelength for remote sensing of ocean aerosols").
const FOAM_ALBEDO: f32 = 0.55;

fn look_foam(
    world_pos: vec3<f32>,
    frag_xy: vec2<f32>,
    view: vec3<f32>,
    normal: vec3<f32>,
    rgb: vec3<f32>,
) -> vec3<f32> {
    let c = foam_coverage_at(world_pos);
    if c <= 0.0 {
        return rgb;
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
    return mix(rgb, foam_lit, c);
}
