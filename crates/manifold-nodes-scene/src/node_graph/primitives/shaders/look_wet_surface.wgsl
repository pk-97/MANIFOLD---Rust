// Look hook: wet surfaces (BUG-l2h3.30.10). Called from `fs_pbr` after the
// material resolves, keyed on world position because materials are shared.
// Water darkens what it touches and lowers its roughness. Until the
// wetness field is bound this returns the surface unchanged.

struct WetSurface {
    albedo: vec3<f32>,
    roughness: f32,
}

fn look_wet_surface(world_pos: vec3<f32>, albedo: vec3<f32>, roughness: f32) -> WetSurface {
    return WetSurface(albedo, roughness);
}
