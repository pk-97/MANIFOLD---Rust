// Look hook: surface foam (BUG-l2h3.30.7). Called from the water shading
// with the shaded surface colour at the hit. Until the foam field is bound
// this returns the colour unchanged.

fn look_foam(world_pos: vec3<f32>, rgb: vec3<f32>) -> vec3<f32> {
    return rgb;
}
