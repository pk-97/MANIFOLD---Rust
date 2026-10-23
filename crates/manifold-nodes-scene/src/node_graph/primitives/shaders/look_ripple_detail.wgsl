// Look hook: ripple detail on the water surface (BUG-l2h3.30.9). Called
// from the water shading with the surface normal at the hit. Until the
// detail normals are bound this returns the normal unchanged.

fn look_ripple_detail(world_pos: vec3<f32>, normal: vec3<f32>) -> vec3<f32> {
    return normal;
}
