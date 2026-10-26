// Look hook: light through thin water. Called from the water shading with
// the light behind the surface and the water thickness it crosses, in
// metres. Until absorption is bound this returns the light unchanged.

fn look_thin_water_light(thickness: f32, rgb: vec3<f32>) -> vec3<f32> {
    return rgb;
}
