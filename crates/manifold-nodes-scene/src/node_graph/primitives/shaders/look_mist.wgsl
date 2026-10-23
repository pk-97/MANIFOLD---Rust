// Look hook: mist from spray (under BUG-l2h3.30). Called from the water
// shading with the colour seen along the ray from `ray_origin` to
// `world_pos`. Until the mist density is bound this returns the colour
// unchanged.

fn look_mist(ray_origin: vec3<f32>, world_pos: vec3<f32>, rgb: vec3<f32>) -> vec3<f32> {
    return rgb;
}
